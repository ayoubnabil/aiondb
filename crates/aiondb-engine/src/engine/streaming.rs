#![allow(clippy::pedantic)]

//! Streaming replication integration for the `AionDB` engine.
//!
//! This module provides the engine-level components for WAL-based streaming
//! replication:
//!
//! - [`StreamingReplicationState`] holds the shared state needed by both the
//!   primary (WAL sender infrastructure) and replica (WAL receiver + replay
//!   loop) sides of the replication protocol.
//! - [`ReplicationManager`] coordinates the lifecycle of replication
//!   connections and provides the interface between the pgwire layer and
//!   the storage engine.
//!
//! # Primary mode
//!
//! When running as a primary:
//! 1. The engine creates a [`WalNotifier`] that is triggered after each
//!    committed transaction.
//! 2. Incoming replication connections are authenticated and registered in
//!    the [`ReplicaRegistry`].
//! 3. Each connection gets a [`WalSender`] that reads WAL entries and
//!    streams them to the replica.
//! 4. The primary uses the minimum flush LSN across all replicas to make
//!    WAL retention decisions.
//!
//! # Replica mode
//!
//! When running as a replica:
//! 1. The engine starts a [`WalReceiver`] that connects to the primary
//!    and receives WAL data.
//! 2. Received WAL entries are appended to local WAL segments.
//! 3. A replay loop reads the local WAL and applies committed transactions
//!    to the storage engine, identical to crash recovery.
//! 4. The replica periodically sends status updates to the primary.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

use aiondb_config::{
    replication::WalCompression as ConfigWalCompression, ReplicationConfig, ReplicationRole,
};
use aiondb_core::{DbError, DbResult};
use aiondb_wal::replication::{
    PhysicalReplicationSlot, ReplicaId, ReplicaRegistry, ReplicaState, ReplicationMessage,
    WalNotifier, WalReceiver, WalReceiverStatusSnapshot, WalSender,
};
use aiondb_wal::{Lsn, WalCompression, WalConfig};
use tracing::info;

// ---------------------------------------------------------------------------
// Streaming replication shared state
// ---------------------------------------------------------------------------

/// Shared state for the streaming replication subsystem.
///
/// Held by the engine and shared with replication connection
/// handlers. Provides thread-safe access to:
/// - The replica registry (primary side)
/// - The WAL notifier (primary side)
/// - The WAL receiver (replica side)
/// - Read-only enforcement state
pub struct StreamingReplicationState {
    /// The server's replication role (encoded as `AtomicU8` for lock-free
    /// runtime transitions).
    role: AtomicU8,

    /// Registry of connected replicas (primary side only).
    replica_registry: Arc<ReplicaRegistry>,

    /// Notification channel for WAL senders (primary side only).
    wal_notifier: Arc<WalNotifier>,

    /// WAL directory path for creating senders.
    wal_dir: PathBuf,

    /// Whether the server is in read-only mode (replica).
    read_only: AtomicBool,

    /// WAL receiver for writing received WAL data (replica side only).
    wal_receiver: Option<Arc<WalReceiver>>,

    /// Replication configuration.
    config: ReplicationConfig,

    /// Current timeline identifier for this server.
    timeline: u64,
}

impl StreamingReplicationState {
    /// Create a new replication state for a primary server.
    pub fn new_primary(wal_dir: PathBuf, config: ReplicationConfig) -> Self {
        Self::new_primary_shared(
            wal_dir,
            config,
            Arc::new(ReplicaRegistry::new()),
            Arc::new(WalNotifier::new(Lsn::ZERO)),
            1,
        )
    }

    /// Create a primary replication state that reuses the shared registry and
    /// notifier owned by the storage layer.
    pub fn new_primary_shared(
        wal_dir: PathBuf,
        config: ReplicationConfig,
        replica_registry: Arc<ReplicaRegistry>,
        wal_notifier: Arc<WalNotifier>,
        timeline: u64,
    ) -> Self {
        Self {
            role: AtomicU8::new(role_to_u8(ReplicationRole::Primary)),
            replica_registry,
            wal_notifier,
            wal_dir,
            read_only: AtomicBool::new(false),
            wal_receiver: None,
            config,
            timeline: timeline.max(1),
        }
    }

    /// Create a new replication state for a replica server.
    pub fn new_replica(
        wal_dir: PathBuf,
        wal_config: WalConfig,
        config: ReplicationConfig,
    ) -> DbResult<Self> {
        let receiver = WalReceiver::open(wal_config)?;
        Ok(Self {
            role: AtomicU8::new(role_to_u8(ReplicationRole::Replica)),
            replica_registry: Arc::new(ReplicaRegistry::new()),
            wal_notifier: Arc::new(WalNotifier::new(Lsn::ZERO)),
            wal_dir,
            read_only: AtomicBool::new(true),
            wal_receiver: Some(Arc::new(receiver)),
            config,
            timeline: 1,
        })
    }

    /// Create a disabled replication state for standalone mode.
    pub fn standalone() -> Self {
        Self {
            role: AtomicU8::new(role_to_u8(ReplicationRole::Standalone)),
            replica_registry: Arc::new(ReplicaRegistry::new()),
            wal_notifier: Arc::new(WalNotifier::new(Lsn::ZERO)),
            wal_dir: PathBuf::new(),
            read_only: AtomicBool::new(false),
            wal_receiver: None,
            config: ReplicationConfig::default(),
            timeline: 1,
        }
    }

    /// Return the replication role.
    pub fn role(&self) -> ReplicationRole {
        role_from_u8(self.role.load(Ordering::Acquire))
    }

    /// Return `true` if the server is in read-only mode.
    pub fn is_read_only(&self) -> bool {
        self.read_only.load(Ordering::Acquire)
    }

    /// Check if a write operation should be allowed.
    pub fn check_writable(&self) -> DbResult<()> {
        if self.is_read_only() {
            Err(DbError::feature_not_supported(
                "cannot execute write operations on a read-only replica server",
            ))
        } else {
            Ok(())
        }
    }

    /// Return a reference to the replica registry (primary side).
    pub fn replica_registry(&self) -> &Arc<ReplicaRegistry> {
        &self.replica_registry
    }

    /// Return a reference to the WAL notifier (primary side).
    pub fn wal_notifier(&self) -> &Arc<WalNotifier> {
        &self.wal_notifier
    }

    /// Return a reference to the WAL receiver (replica side).
    pub fn wal_receiver(&self) -> Option<&Arc<WalReceiver>> {
        self.wal_receiver.as_ref()
    }

    /// Notify WAL senders that new WAL data is available.
    ///
    /// Called by the engine after each committed transaction.
    pub fn notify_wal_advance(&self, lsn: Lsn) {
        self.wal_notifier.notify_new_wal(lsn);
    }

    /// Return the minimum flush LSN across all connected replicas.
    ///
    /// Returns `None` if no replicas are connected.
    pub fn min_replica_flush_lsn(&self) -> Option<Lsn> {
        self.replica_registry.min_flush_lsn()
    }

    /// Return a snapshot of all connected replica states.
    pub fn replica_states(&self) -> Vec<ReplicaState> {
        self.replica_registry.snapshot()
    }

    /// Return the number of connected replicas.
    pub fn replica_count(&self) -> usize {
        self.replica_registry.count()
    }

    /// Return the replication configuration.
    pub fn config(&self) -> &ReplicationConfig {
        &self.config
    }

    /// Return the current timeline identifier.
    pub fn timeline(&self) -> u64 {
        self.timeline
    }

    /// Return the WAL directory path. Used by `BASE_BACKUP` to stream
    /// segment files and by tests that need to inspect on-disk state.
    pub fn wal_dir(&self) -> &std::path::Path {
        &self.wal_dir
    }
}

impl StreamingReplicationState {
    /// Promote this replica to primary.
    ///
    /// Disables read-only mode and updates the role atomically.
    /// Returns an error if this node is not currently a replica.
    pub fn promote_to_primary(&self) -> DbResult<()> {
        let current = self.role();
        if current != ReplicationRole::Replica {
            return Err(DbError::feature_not_supported(format!(
                "cannot promote a {} node; only replicas can be promoted to primary",
                current.as_str()
            )));
        }
        self.read_only.store(false, Ordering::Release);
        self.role
            .store(role_to_u8(ReplicationRole::Primary), Ordering::Release);
        info!("node promoted from replica to primary");
        Ok(())
    }

    /// Demote this primary to a replica.
    ///
    /// Enables read-only mode and updates the role atomically.
    /// Returns an error if this node is not currently a primary.
    pub fn demote_to_replica(&self) -> DbResult<()> {
        let current = self.role();
        if current != ReplicationRole::Primary {
            return Err(DbError::feature_not_supported(format!(
                "cannot demote a {} node; only primaries can be demoted to replica",
                current.as_str()
            )));
        }
        self.read_only.store(true, Ordering::Release);
        self.role
            .store(role_to_u8(ReplicationRole::Replica), Ordering::Release);
        info!("node demoted from primary to replica");
        Ok(())
    }

    /// Transition to standalone mode.
    ///
    /// Disables read-only mode and sets the role to standalone.
    pub fn transition_to_standalone(&self) {
        self.read_only.store(false, Ordering::Release);
        self.role
            .store(role_to_u8(ReplicationRole::Standalone), Ordering::Release);
        info!("node transitioned to standalone mode");
    }
}

impl std::fmt::Debug for StreamingReplicationState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamingReplicationState")
            .field("role", &self.role())
            .field("read_only", &self.is_read_only())
            .field("replica_count", &self.replica_count())
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Replication Manager
// ---------------------------------------------------------------------------

/// Manages the lifecycle of streaming replication for the engine.
///
/// On the primary side, it creates WAL senders for incoming replication
/// connections. On the replica side, it manages the WAL receiver and
/// replay loop.
pub struct ReplicationManager {
    state: Arc<StreamingReplicationState>,
}

impl ReplicationManager {
    /// Create a new replication manager wrapping the given state.
    pub fn new(state: Arc<StreamingReplicationState>) -> Self {
        Self { state }
    }

    /// Create a WAL sender for a new replica connection.
    ///
    /// Registers the replica in the registry and returns a sender that
    /// reads WAL entries from the given start LSN.
    pub fn create_wal_sender(&self, start_lsn: Lsn) -> DbResult<(WalSender, ReplicaId)> {
        self.create_wal_sender_for_slot(start_lsn, None)
    }

    /// Create a WAL sender for a new replica connection bound to an optional
    /// physical replication slot.
    pub fn create_wal_sender_for_slot(
        &self,
        start_lsn: Lsn,
        slot_name: Option<&str>,
    ) -> DbResult<(WalSender, ReplicaId)> {
        if self.state.role() != ReplicationRole::Primary {
            return Err(DbError::feature_not_supported(
                "this server is not configured as a replication primary",
            ));
        }

        if let Some(slot_name) = slot_name {
            if let Some(slot) = self
                .state
                .replica_registry()
                .read_physical_slot(slot_name)?
            {
                let current_timeline = self.state.timeline();
                if slot.restart_tli != current_timeline {
                    return Err(DbError::feature_not_supported(format!(
                        "physical replication slot \"{slot_name}\" is on timeline {}, but current timeline is {}; use TIMELINE_HISTORY and a slot on the active timeline",
                        slot.restart_tli,
                        current_timeline
                    )));
                }
            }
        }

        let max_senders =
            usize::try_from(self.state.config().max_wal_senders).unwrap_or(usize::MAX);
        let replica_id = self.state.replica_registry().try_register_with_slot(
            start_lsn,
            slot_name,
            max_senders,
        )?;
        let sender = WalSender::new_with_compression(
            self.state.wal_dir.clone(),
            start_lsn,
            replica_id,
            wal_compression_for_sender(self.state.config().wal_compression),
        );

        info!(
            replica_id = %replica_id,
            start_lsn = start_lsn.get(),
            "registered new replication connection"
        );

        Ok((sender, replica_id))
    }

    /// Create a new physical replication slot on the primary.
    pub fn create_physical_slot(
        &self,
        slot_name: &str,
        reserve_wal: bool,
    ) -> DbResult<PhysicalReplicationSlot> {
        if self.state.role() != ReplicationRole::Primary {
            return Err(DbError::feature_not_supported(
                "this server is not configured as a replication primary",
            ));
        }
        let current_lsn = self.state.wal_notifier().current_lsn();
        self.state.replica_registry().create_physical_slot(
            slot_name,
            current_lsn,
            reserve_wal,
            self.state.timeline(),
        )
    }

    /// Read a physical replication slot from the shared registry.
    pub fn read_physical_slot(&self, slot_name: &str) -> DbResult<Option<PhysicalReplicationSlot>> {
        self.state.replica_registry().read_physical_slot(slot_name)
    }

    /// Unregister a replica when it disconnects.
    pub fn disconnect_replica(&self, replica_id: ReplicaId) {
        info!(replica_id = %replica_id, "replication connection closed");
        self.state.replica_registry().unregister(replica_id);
    }

    /// Return a reference to the shared replication state.
    pub fn state(&self) -> &Arc<StreamingReplicationState> {
        &self.state
    }

    fn require_wal_receiver(&self) -> DbResult<&Arc<WalReceiver>> {
        self.state
            .wal_receiver()
            .ok_or_else(|| DbError::internal("WAL receiver not available in this mode"))
    }

    /// Process received WAL data on the replica side.
    ///
    /// Writes the received entries to local WAL storage.
    pub fn receive_wal_data(&self, data: &[u8]) -> DbResult<Lsn> {
        self.require_wal_receiver()?.receive_wal_data(data)
    }

    /// Process a received replication message on the replica side.
    ///
    /// Only `ReplicationMessage::WalData` is accepted here. Message metadata
    /// is validated against the decoded WAL payload before local writes.
    pub fn receive_replication_message(&self, message: &ReplicationMessage) -> DbResult<Lsn> {
        self.require_wal_receiver()?.receive_message(message)
    }

    /// Validate that the upstream primary identity matches the local replica
    /// metadata before starting streaming or applying WAL.
    pub fn validate_upstream_identity(
        &self,
        system_identifier: &str,
        timeline: u32,
    ) -> DbResult<()> {
        let system_identifier = system_identifier.parse::<u64>().map_err(|error| {
            DbError::internal(format!(
                "invalid upstream replication system identifier \"{system_identifier}\": {error}"
            ))
        })?;
        self.require_wal_receiver()?
            .validate_upstream_identity(system_identifier, timeline)
    }

    /// Flush the replica's local WAL to durable storage.
    pub fn flush_replica_wal(&self) -> DbResult<Lsn> {
        self.require_wal_receiver()?.flush_durable()
    }

    /// Build a standby status update message from the replica's current
    /// progress.
    pub fn replica_status_update(&self) -> DbResult<ReplicationMessage> {
        Ok(self.require_wal_receiver()?.status_update())
    }

    pub fn wal_receiver_status_snapshot(&self) -> DbResult<WalReceiverStatusSnapshot> {
        Ok(self.require_wal_receiver()?.status_snapshot())
    }

    /// Record that WAL entries up to `lsn` have been replayed.
    pub fn set_replica_apply_lsn(&self, lsn: Lsn) {
        if let Some(receiver) = self.state.wal_receiver() {
            receiver.set_apply_lsn(lsn);
        }
    }
}

impl std::fmt::Debug for ReplicationManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplicationManager")
            .field("state", &self.state)
            .finish()
    }
}

fn role_to_u8(role: ReplicationRole) -> u8 {
    match role {
        ReplicationRole::Standalone => 0,
        ReplicationRole::Primary => 1,
        ReplicationRole::Replica => 2,
    }
}

fn role_from_u8(val: u8) -> ReplicationRole {
    match val {
        1 => ReplicationRole::Primary,
        2 => ReplicationRole::Replica,
        _ => ReplicationRole::Standalone,
    }
}

fn wal_compression_for_sender(mode: ConfigWalCompression) -> WalCompression {
    match mode {
        ConfigWalCompression::None => WalCompression::None,
        ConfigWalCompression::Lz4 => WalCompression::Lz4,
        ConfigWalCompression::Zstd => WalCompression::Zstd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn replication_test_root(name: &str) -> PathBuf {
        let root = crate::test_support::unique_temp_path("engine-streaming-tests", name);
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn create_wal_sender_respects_max_wal_senders_limit() {
        let mut config = ReplicationConfig::default();
        config.role = ReplicationRole::Primary;
        config.max_wal_senders = 1;
        let root = replication_test_root("sender-limit");
        let state = Arc::new(StreamingReplicationState::new_primary(root.clone(), config));
        let manager = ReplicationManager::new(state);

        let (_sender, replica_id) = manager
            .create_wal_sender(Lsn::new(1))
            .expect("first WAL sender should be admitted");
        let err = manager
            .create_wal_sender(Lsn::new(1))
            .expect_err("second WAL sender must be rejected at the configured limit");
        assert!(err
            .to_string()
            .contains("maximum number of WAL senders (1) reached"));

        manager.disconnect_replica(replica_id);

        manager
            .create_wal_sender(Lsn::new(2))
            .expect("sender slot should be reusable after disconnect");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn create_wal_sender_for_slot_rejects_timeline_mismatch() {
        let mut config = ReplicationConfig::default();
        config.role = ReplicationRole::Primary;

        let registry = Arc::new(ReplicaRegistry::new());
        registry
            .create_physical_slot("slot_a", Lsn::new(10), true, 1)
            .expect("slot should be creatable");
        let root = replication_test_root("slot-timeline-mismatch");

        let state = Arc::new(StreamingReplicationState::new_primary_shared(
            root.clone(),
            config,
            Arc::clone(&registry),
            Arc::new(WalNotifier::new(Lsn::ZERO)),
            2,
        ));
        let manager = ReplicationManager::new(state);

        let err = manager
            .create_wal_sender_for_slot(Lsn::new(10), Some("slot_a"))
            .expect_err("slot on old timeline must be rejected");
        assert!(err.to_string().contains("timeline"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn create_wal_sender_for_slot_accepts_matching_timeline() {
        let mut config = ReplicationConfig::default();
        config.role = ReplicationRole::Primary;

        let registry = Arc::new(ReplicaRegistry::new());
        registry
            .create_physical_slot("slot_a", Lsn::new(10), true, 2)
            .expect("slot should be creatable");
        let root = replication_test_root("slot-timeline-match");

        let state = Arc::new(StreamingReplicationState::new_primary_shared(
            root.clone(),
            config,
            Arc::clone(&registry),
            Arc::new(WalNotifier::new(Lsn::ZERO)),
            2,
        ));
        let manager = ReplicationManager::new(state);

        let (_sender, replica_id) = manager
            .create_wal_sender_for_slot(Lsn::new(10), Some("slot_a"))
            .expect("slot on current timeline should be accepted");
        manager.disconnect_replica(replica_id);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn validate_upstream_identity_accepts_matching_replica_metadata() {
        let root = replication_test_root("identity-match");
        let wal_dir = root.join("wal");
        let replication_dir = root.join("replication");
        std::fs::create_dir_all(&replication_dir).expect("create replication metadata dir");
        std::fs::write(replication_dir.join("system_id"), b"42").expect("write system id");
        std::fs::write(replication_dir.join("timeline"), b"7").expect("write timeline");

        let mut config = ReplicationConfig::default();
        config.role = ReplicationRole::Replica;
        let state = Arc::new(
            StreamingReplicationState::new_replica(
                wal_dir.clone(),
                WalConfig {
                    dir: wal_dir.clone(),
                    ..WalConfig::default()
                },
                config,
            )
            .expect("open replica state"),
        );
        let manager = ReplicationManager::new(state);

        manager
            .validate_upstream_identity("42", 7)
            .expect("matching identity should succeed");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn validate_upstream_identity_rejects_invalid_system_identifier_text() {
        let root = replication_test_root("identity-invalid");
        let wal_dir = root.join("wal");

        let mut config = ReplicationConfig::default();
        config.role = ReplicationRole::Replica;
        let state = Arc::new(
            StreamingReplicationState::new_replica(
                wal_dir.clone(),
                WalConfig {
                    dir: wal_dir.clone(),
                    ..WalConfig::default()
                },
                config,
            )
            .expect("open replica state"),
        );
        let manager = ReplicationManager::new(state);

        let error = manager
            .validate_upstream_identity("not-a-number", 1)
            .expect_err("non numeric system identifier must fail");
        assert!(error
            .to_string()
            .contains("invalid upstream replication system identifier"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn receive_replication_message_applies_valid_wal_data() {
        let root = replication_test_root("message-apply");
        let wal_dir = root.join("wal");

        let mut config = ReplicationConfig::default();
        config.role = ReplicationRole::Replica;
        let state = Arc::new(
            StreamingReplicationState::new_replica(
                wal_dir.clone(),
                WalConfig {
                    dir: wal_dir.clone(),
                    wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
                    ..WalConfig::default()
                },
                config,
            )
            .expect("open replica state"),
        );
        let manager = ReplicationManager::new(state);

        let message = ReplicationMessage::WalData {
            start_lsn: Lsn::new(1),
            end_lsn: Lsn::new(1),
            data: aiondb_wal::codec::encode_entry(&aiondb_wal::record::WalEntry {
                lsn: Lsn::new(1),
                prev_lsn: Lsn::ZERO,
                database_id: aiondb_wal::record::WalEntry::LEGACY_DATABASE_ID,
                record: aiondb_wal::WalRecord::Checkpoint {
                    last_committed_lsn: Lsn::ZERO,
                },
            })
            .expect("encode wal entry"),
        };

        let last_lsn = manager
            .receive_replication_message(&message)
            .expect("valid WalData should be applied");
        assert_eq!(last_lsn, Lsn::new(1));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn receive_replication_message_rejects_non_wal_data_messages() {
        let root = replication_test_root("message-reject");
        let wal_dir = root.join("wal");

        let mut config = ReplicationConfig::default();
        config.role = ReplicationRole::Replica;
        let state = Arc::new(
            StreamingReplicationState::new_replica(
                wal_dir.clone(),
                WalConfig {
                    dir: wal_dir.clone(),
                    ..WalConfig::default()
                },
                config,
            )
            .expect("open replica state"),
        );
        let manager = ReplicationManager::new(state);

        let error = manager
            .receive_replication_message(&ReplicationMessage::StandbyStatusUpdate {
                write_lsn: Lsn::ZERO,
                flush_lsn: Lsn::ZERO,
                apply_lsn: Lsn::ZERO,
            })
            .expect_err("non WalData replication messages must be rejected");
        assert!(error.to_string().contains("expected WalData or Keepalive"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn wal_receiver_status_snapshot_surfaces_replica_progress() {
        let root = replication_test_root("snapshot-progress");
        let wal_dir = root.join("wal");
        let replication_dir = root.join("replication");
        std::fs::create_dir_all(&replication_dir).expect("create replication metadata dir");
        std::fs::write(replication_dir.join("system_id"), b"42").expect("write system id");
        std::fs::write(replication_dir.join("timeline"), b"7").expect("write timeline");

        let mut config = ReplicationConfig::default();
        config.role = ReplicationRole::Replica;
        let state = Arc::new(
            StreamingReplicationState::new_replica(
                wal_dir.clone(),
                WalConfig {
                    dir: wal_dir.clone(),
                    wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
                    ..WalConfig::default()
                },
                config,
            )
            .expect("open replica state"),
        );
        let manager = ReplicationManager::new(state);

        let message = ReplicationMessage::WalData {
            start_lsn: Lsn::new(1),
            end_lsn: Lsn::new(1),
            data: aiondb_wal::codec::encode_entry(&aiondb_wal::record::WalEntry {
                lsn: Lsn::new(1),
                prev_lsn: Lsn::ZERO,
                database_id: aiondb_wal::record::WalEntry::LEGACY_DATABASE_ID,
                record: aiondb_wal::WalRecord::Checkpoint {
                    last_committed_lsn: Lsn::ZERO,
                },
            })
            .expect("encode wal entry"),
        };
        manager
            .receive_replication_message(&message)
            .expect("apply wal data for snapshot");
        manager.flush_replica_wal().expect("flush wal for snapshot");
        manager.set_replica_apply_lsn(Lsn::new(1));

        let snapshot = manager
            .wal_receiver_status_snapshot()
            .expect("receiver snapshot should be available");
        assert_eq!(snapshot.write_lsn, Lsn::new(1));
        assert_eq!(snapshot.flush_lsn, Lsn::new(1));
        assert_eq!(snapshot.apply_lsn, Lsn::new(1));
        assert_eq!(snapshot.local_system_identifier, Some(42));
        assert_eq!(snapshot.local_timeline_id, Some(7));

        let _ = std::fs::remove_dir_all(root);
    }
}
