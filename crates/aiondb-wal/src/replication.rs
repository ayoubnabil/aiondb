//! WAL-based streaming replication protocol.
//!
//! Replication primitives between the WAL subsystem and the network layer:
//!
//! - [`WalSender`] reads WAL entries starting from a given LSN and streams
//!   them as encoded byte chunks to a consumer (the pgwire replication
//!   connection).
//! - [`WalReceiver`] accepts encoded WAL byte chunks from a primary and
//!   writes them into the local WAL segment directory for subsequent replay.
//! - [`ReplicaState`] tracks each replica's confirmed replay position and
//!   provides the primary with feedback for WAL retention decisions.
//!
//! # Wire format
//!
//! Replication messages are framed with a simple envelope:
//!
//! ```text
//! msg_type (u8) | payload_len (u32 LE) | payload (payload_len bytes)
//! ```
//!
//! Message types:
//! - `0x01` **`WalData`** -- a batch of encoded WAL entries
//! - `0x02` **`StandbyStatusUpdate`** -- replica reports its replay position
//! - `0x03` **Keepalive** -- primary heartbeat (replica should reply if
//!   `reply_requested` is set)
//! - `0x04` **`KeepaliveReply`** -- replica response to a keepalive

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::{Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use aiondb_core::{convert::usize_to_u64_saturating, DbError, DbResult};
use time::OffsetDateTime;

use crate::codec;
use crate::lsn::Lsn;
use crate::reader::WalReader;
use crate::segment;
use crate::writer::WalWriter;
use crate::{WalCompression, WalConfig};

mod notifier;
mod slot_persistence;

pub use self::notifier::WalNotifier;
use self::slot_persistence::{
    read_slot_file, slot_metadata_path, sync_dir, sync_parent_dir, validate_slot_name,
    write_slot_file_atomically,
};

// ---------------------------------------------------------------------------
// Replication message types
// ---------------------------------------------------------------------------

/// Tag byte for a `WalData` message containing one or more encoded WAL entries.
pub const MSG_WAL_DATA: u8 = 0x01;
/// Tag byte for a standby status update from replica to primary.
pub const MSG_STANDBY_STATUS_UPDATE: u8 = 0x02;
/// Tag byte for a keepalive from primary to replica.
pub const MSG_KEEPALIVE: u8 = 0x03;
/// Tag byte for a keepalive reply from replica to primary.
pub const MSG_KEEPALIVE_REPLY: u8 = 0x04;

/// Maximum number of WAL entries sent in a single `WalData` message.
const MAX_ENTRIES_PER_BATCH: usize = 1024;

/// Maximum byte size of a single `WalData` payload before flushing a batch.
const MAX_BATCH_BYTES: usize = 1024 * 1024; // 1 MiB
/// Maximum payload accepted when decoding replication frames.
///
/// Keeps memory usage bounded when parsing untrusted network input.
const MAX_REPLICATION_MESSAGE_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const MAX_SYNC_COMMIT_WAIT_CHUNK: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Replication message encoding/decoding
// ---------------------------------------------------------------------------

/// A replication protocol message.
#[derive(Clone, Debug, PartialEq)]
pub enum ReplicationMessage {
    /// A batch of encoded WAL entries sent from primary to replica.
    WalData {
        /// The LSN of the first entry in this batch.
        start_lsn: Lsn,
        /// The LSN of the last entry in this batch.
        end_lsn: Lsn,
        /// The raw encoded WAL entries (concatenated `encode_entry` output).
        data: Vec<u8>,
    },
    /// Replica reports its replay progress to the primary.
    StandbyStatusUpdate {
        /// The last WAL entry LSN that has been written to local WAL storage.
        write_lsn: Lsn,
        /// The last WAL entry LSN that has been flushed (durable) locally.
        flush_lsn: Lsn,
        /// The last WAL entry LSN that has been replayed into the local
        /// storage engine.
        apply_lsn: Lsn,
    },
    /// Heartbeat from primary. If `reply_requested` is `true`, the replica
    /// should respond with a `StandbyStatusUpdate`.
    Keepalive {
        /// The primary's current end-of-WAL LSN.
        wal_end: Lsn,
        /// Monotonic timestamp in microseconds since an arbitrary epoch.
        timestamp_us: u64,
        /// Whether the primary expects an immediate reply.
        reply_requested: bool,
    },
    /// Reply to a keepalive from the replica.
    KeepaliveReply {
        /// Monotonic timestamp in microseconds.
        timestamp_us: u64,
    },
}

fn checked_replication_payload_len(kind: &str, parts: &[usize]) -> DbResult<usize> {
    let mut total = 0usize;
    for part in parts {
        total = total.checked_add(*part).ok_or_else(|| {
            DbError::internal(format!(
                "replication: {kind} payload length overflowed usize"
            ))
        })?;
    }
    replication_payload_len(kind, total)?;
    Ok(total)
}

fn replication_payload_len(kind: &str, payload_len: usize) -> DbResult<u32> {
    if payload_len > MAX_REPLICATION_MESSAGE_PAYLOAD_BYTES {
        return Err(DbError::internal(format!(
            "replication: {kind} payload too large ({payload_len} bytes, max {MAX_REPLICATION_MESSAGE_PAYLOAD_BYTES})"
        )));
    }
    u32::try_from(payload_len).map_err(|_| {
        DbError::internal(format!(
            "replication: {kind} payload exceeds u32 frame length ({payload_len} bytes)"
        ))
    })
}

impl ReplicationMessage {
    /// Encode this message into a framed byte buffer.
    ///
    /// Format: `msg_type (u8) | payload_len (u32 LE) | payload`.
    pub fn encode(&self) -> DbResult<Vec<u8>> {
        let (tag, payload) = match self {
            Self::WalData {
                start_lsn,
                end_lsn,
                data,
            } => {
                if start_lsn > end_lsn {
                    return Err(DbError::internal(format!(
                        "replication: WalData start_lsn {} exceeds end_lsn {}",
                        start_lsn.get(),
                        end_lsn.get()
                    )));
                }
                if data.is_empty() {
                    return Err(DbError::internal(
                        "replication: WalData payload contains no WAL entries",
                    ));
                }
                let payload_capacity =
                    checked_replication_payload_len("WalData", &[16, data.len()])?;
                let mut payload = Vec::with_capacity(payload_capacity);
                payload.extend_from_slice(&start_lsn.get().to_le_bytes());
                payload.extend_from_slice(&end_lsn.get().to_le_bytes());
                payload.extend_from_slice(data);
                (MSG_WAL_DATA, payload)
            }
            Self::StandbyStatusUpdate {
                write_lsn,
                flush_lsn,
                apply_lsn,
            } => {
                if flush_lsn > write_lsn {
                    return Err(DbError::internal(format!(
                        "replication: flush_lsn {} exceeds write_lsn {}",
                        flush_lsn.get(),
                        write_lsn.get()
                    )));
                }
                if apply_lsn > flush_lsn {
                    return Err(DbError::internal(format!(
                        "replication: apply_lsn {} exceeds flush_lsn {}",
                        apply_lsn.get(),
                        flush_lsn.get()
                    )));
                }
                let mut payload = Vec::with_capacity(24);
                payload.extend_from_slice(&write_lsn.get().to_le_bytes());
                payload.extend_from_slice(&flush_lsn.get().to_le_bytes());
                payload.extend_from_slice(&apply_lsn.get().to_le_bytes());
                (MSG_STANDBY_STATUS_UPDATE, payload)
            }
            Self::Keepalive {
                wal_end,
                timestamp_us,
                reply_requested,
            } => {
                let mut payload = Vec::with_capacity(17);
                payload.extend_from_slice(&wal_end.get().to_le_bytes());
                payload.extend_from_slice(&timestamp_us.to_le_bytes());
                payload.push(u8::from(*reply_requested));
                (MSG_KEEPALIVE, payload)
            }
            Self::KeepaliveReply { timestamp_us } => {
                let payload = timestamp_us.to_le_bytes().to_vec();
                (MSG_KEEPALIVE_REPLY, payload)
            }
        };

        let payload_len = replication_payload_len("replication message", payload.len())?;
        let mut buf = Vec::with_capacity(1 + 4 + payload.len());
        buf.push(tag);
        buf.extend_from_slice(&payload_len.to_le_bytes());
        buf.extend_from_slice(&payload);
        Ok(buf)
    }

    /// Decode a replication message from a framed byte buffer.
    ///
    /// Returns `(message, bytes_consumed)`.
    pub fn decode(data: &[u8]) -> DbResult<(Self, usize)> {
        if data.len() < 5 {
            return Err(DbError::internal(
                "replication: not enough data for message header",
            ));
        }

        let tag = data[0];
        let payload_len = usize::try_from(u32::from_le_bytes([data[1], data[2], data[3], data[4]]))
            .map_err(|_| DbError::internal("replication: payload length overflow"))?;
        if payload_len > MAX_REPLICATION_MESSAGE_PAYLOAD_BYTES {
            return Err(DbError::internal(format!(
                "replication: payload too large ({payload_len} bytes, max {MAX_REPLICATION_MESSAGE_PAYLOAD_BYTES})"
            )));
        }

        let total = 5usize
            .checked_add(payload_len)
            .ok_or_else(|| DbError::internal("replication: message length overflow"))?;
        if data.len() < total {
            return Err(DbError::internal("replication: truncated message payload"));
        }

        let payload = &data[5..total];

        let msg = match tag {
            MSG_WAL_DATA => {
                if payload.len() < 16 {
                    return Err(DbError::internal("replication: WalData payload too short"));
                }
                let start_lsn = Lsn::new(u64::from_le_bytes(payload[0..8].try_into().map_err(
                    |e| {
                        DbError::internal(format!(
                            "replication: invalid WalData start_lsn conversion: {e}"
                        ))
                    },
                )?));
                let end_lsn = Lsn::new(u64::from_le_bytes(payload[8..16].try_into().map_err(
                    |e| {
                        DbError::internal(format!(
                            "replication: invalid WalData end_lsn conversion: {e}"
                        ))
                    },
                )?));
                if start_lsn > end_lsn {
                    return Err(DbError::internal(format!(
                        "replication: WalData start_lsn {} exceeds end_lsn {}",
                        start_lsn.get(),
                        end_lsn.get()
                    )));
                }
                let wal_data = payload[16..].to_vec();
                if wal_data.is_empty() {
                    return Err(DbError::internal(
                        "replication: WalData payload contains no WAL entries",
                    ));
                }
                Self::WalData {
                    start_lsn,
                    end_lsn,
                    data: wal_data,
                }
            }
            MSG_STANDBY_STATUS_UPDATE => {
                if payload.len() != 24 {
                    return Err(DbError::internal(
                        "replication: StandbyStatusUpdate payload length must be exactly 24 bytes",
                    ));
                }
                let write_lsn = Lsn::new(u64::from_le_bytes(payload[0..8].try_into().map_err(
                    |e| {
                        DbError::internal(format!(
                            "replication: invalid StandbyStatusUpdate write_lsn conversion: {e}"
                        ))
                    },
                )?));
                let flush_lsn = Lsn::new(u64::from_le_bytes(payload[8..16].try_into().map_err(
                    |e| {
                        DbError::internal(format!(
                            "replication: invalid StandbyStatusUpdate flush_lsn conversion: {e}"
                        ))
                    },
                )?));
                let apply_lsn = Lsn::new(u64::from_le_bytes(payload[16..24].try_into().map_err(
                    |e| {
                        DbError::internal(format!(
                            "replication: invalid StandbyStatusUpdate apply_lsn conversion: {e}"
                        ))
                    },
                )?));
                if flush_lsn > write_lsn {
                    return Err(DbError::internal(format!(
                        "replication: StandbyStatusUpdate flush_lsn {} exceeds write_lsn {}",
                        flush_lsn.get(),
                        write_lsn.get()
                    )));
                }
                if apply_lsn > flush_lsn {
                    return Err(DbError::internal(format!(
                        "replication: StandbyStatusUpdate apply_lsn {} exceeds flush_lsn {}",
                        apply_lsn.get(),
                        flush_lsn.get()
                    )));
                }
                Self::StandbyStatusUpdate {
                    write_lsn,
                    flush_lsn,
                    apply_lsn,
                }
            }
            MSG_KEEPALIVE => {
                if payload.len() != 17 {
                    return Err(DbError::internal(
                        "replication: Keepalive payload length must be exactly 17 bytes",
                    ));
                }
                let wal_end = Lsn::new(u64::from_le_bytes(payload[0..8].try_into().map_err(
                    |e| {
                        DbError::internal(format!(
                            "replication: invalid Keepalive wal_end conversion: {e}"
                        ))
                    },
                )?));
                let timestamp_us = u64::from_le_bytes(payload[8..16].try_into().map_err(|e| {
                    DbError::internal(format!(
                        "replication: invalid Keepalive timestamp_us conversion: {e}"
                    ))
                })?);
                let reply_requested = match payload[16] {
                    0 => false,
                    1 => true,
                    other => {
                        return Err(DbError::internal(format!(
                            "replication: invalid Keepalive reply_requested flag {other}"
                        )));
                    }
                };
                Self::Keepalive {
                    wal_end,
                    timestamp_us,
                    reply_requested,
                }
            }
            MSG_KEEPALIVE_REPLY => {
                if payload.len() != 8 {
                    return Err(DbError::internal(
                        "replication: KeepaliveReply payload length must be exactly 8 bytes",
                    ));
                }
                let timestamp_us = u64::from_le_bytes(payload[0..8].try_into().map_err(|e| {
                    DbError::internal(format!(
                        "replication: invalid KeepaliveReply timestamp_us conversion: {e}"
                    ))
                })?);
                Self::KeepaliveReply { timestamp_us }
            }
            _ => {
                return Err(DbError::internal(format!(
                    "replication: unknown message tag {tag:#04x}"
                )));
            }
        };

        Ok((msg, total))
    }
}

// ---------------------------------------------------------------------------
// Replica tracking (primary side)
// ---------------------------------------------------------------------------

/// Identifier for a connected replica.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct ReplicaId(u64);

impl ReplicaId {
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ReplicaId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "replica-{}", self.0)
    }
}

/// Per-replica state tracked by the primary.
#[derive(Clone, Debug)]
pub struct ReplicaState {
    /// Unique identifier for this replica connection.
    pub id: ReplicaId,
    /// Last WAL LSN written to the replica's local WAL storage.
    pub write_lsn: Lsn,
    /// Last WAL LSN flushed (durable) on the replica.
    pub flush_lsn: Lsn,
    /// Last WAL LSN replayed into the replica's storage engine.
    pub apply_lsn: Lsn,
    /// Application name reported by the replica during startup.
    pub application_name: Option<String>,
    /// The LSN from which this replica started streaming.
    pub start_lsn: Lsn,
    /// Optional physical replication slot associated with this connection.
    pub slot_name: Option<String>,
}

impl ReplicaState {
    pub fn new(id: ReplicaId, start_lsn: Lsn, slot_name: Option<String>) -> Self {
        Self {
            id,
            write_lsn: Lsn::ZERO,
            flush_lsn: Lsn::ZERO,
            apply_lsn: Lsn::ZERO,
            application_name: None,
            start_lsn,
            slot_name,
        }
    }

    /// Update the replica's progress from a standby status update.
    pub fn update_progress(&mut self, write_lsn: Lsn, flush_lsn: Lsn, apply_lsn: Lsn) {
        let write_lsn = write_lsn.max(self.write_lsn);
        let flush_lsn = flush_lsn.max(self.flush_lsn).min(write_lsn);
        let apply_lsn = apply_lsn.max(self.apply_lsn).min(flush_lsn);
        self.write_lsn = write_lsn;
        self.flush_lsn = flush_lsn;
        self.apply_lsn = apply_lsn;
    }

    /// Return the earliest LSN this replica still needs for catch-up.
    pub fn retention_lsn(&self) -> Lsn {
        self.flush_lsn.max(self.start_lsn)
    }
}

/// Persisted state for a physical replication slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhysicalReplicationSlot {
    /// Unique slot name.
    pub name: String,
    /// Oldest WAL LSN that must be retained for this slot.
    pub restart_lsn: Option<Lsn>,
    /// Timeline associated with `restart_lsn`.
    pub restart_tli: u64,
}

impl PhysicalReplicationSlot {
    fn new(name: impl Into<String>, restart_lsn: Option<Lsn>, restart_tli: u64) -> Self {
        Self {
            name: name.into(),
            restart_lsn,
            restart_tli,
        }
    }
}

/// Registry of all connected replicas, used by the primary to track
/// replication progress and make WAL retention decisions.
#[derive(Debug)]
pub struct ReplicaRegistry {
    inner: RwLock<ReplicaRegistryInner>,
    slot_dir: Option<PathBuf>,
    sync_commit_cv: Condvar,
    sync_commit_lock: Mutex<()>,
}

#[derive(Debug)]
struct ReplicaRegistryInner {
    replicas: HashMap<ReplicaId, ReplicaState>,
    slots: HashMap<String, PersistentPhysicalReplicationSlot>,
    next_id: u64,
}

#[derive(Clone, Debug)]
struct PersistentPhysicalReplicationSlot {
    slot: PhysicalReplicationSlot,
    active_replica_id: Option<ReplicaId>,
}

impl PersistentPhysicalReplicationSlot {
    fn new(slot: PhysicalReplicationSlot) -> Self {
        Self {
            slot,
            active_replica_id: None,
        }
    }
}

impl ReplicaRegistry {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(ReplicaRegistryInner {
                replicas: HashMap::new(),
                slots: HashMap::new(),
                next_id: 1,
            }),
            slot_dir: None,
            sync_commit_cv: Condvar::new(),
            sync_commit_lock: Mutex::new(()),
        }
    }

    /// Open a replica registry backed by persistent physical slot metadata.
    pub fn open(slot_dir: PathBuf) -> DbResult<Self> {
        fs::create_dir_all(&slot_dir).map_err(|error| {
            DbError::internal(format!(
                "failed to create replication slot directory {}: {error}",
                slot_dir.display()
            ))
        })?;
        sync_dir(&slot_dir)?;
        sync_parent_dir(&slot_dir, "replication slot directory")?;

        let mut slots = HashMap::new();
        for entry in fs::read_dir(&slot_dir).map_err(|error| {
            DbError::internal(format!(
                "failed to enumerate replication slot directory {}: {error}",
                slot_dir.display()
            ))
        })? {
            let entry = entry.map_err(|error| {
                DbError::internal(format!(
                    "failed to enumerate replication slot directory {}: {error}",
                    slot_dir.display()
                ))
            })?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let slot = read_slot_file(&path)?;
            let expected_name =
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .ok_or_else(|| {
                        DbError::internal(format!(
                            "replication slot metadata {} has invalid filename",
                            path.display()
                        ))
                    })?;
            if slot.name != expected_name {
                return Err(DbError::internal(format!(
                    "replication slot metadata filename/name mismatch: {} declares slot_name \"{}\"",
                    path.display(),
                    slot.name
                )));
            }
            if slots.contains_key(&slot.name) {
                return Err(DbError::internal(format!(
                    "duplicate replication slot metadata for \"{}\"",
                    slot.name
                )));
            }
            slots.insert(
                slot.name.clone(),
                PersistentPhysicalReplicationSlot::new(slot),
            );
        }

        Ok(Self {
            inner: RwLock::new(ReplicaRegistryInner {
                replicas: HashMap::new(),
                slots,
                next_id: 1,
            }),
            slot_dir: Some(slot_dir),
            sync_commit_cv: Condvar::new(),
            sync_commit_lock: Mutex::new(()),
        })
    }

    fn read_inner(&self) -> std::sync::RwLockReadGuard<'_, ReplicaRegistryInner> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write_inner(&self) -> std::sync::RwLockWriteGuard<'_, ReplicaRegistryInner> {
        self.inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Register a new replica connection and return its assigned ID.
    pub fn register(&self, start_lsn: Lsn) -> DbResult<ReplicaId> {
        self.try_register(start_lsn, usize::MAX)
    }

    /// Register a new replica connection while enforcing a hard upper bound
    /// on the number of concurrently connected replicas.
    pub fn try_register(&self, start_lsn: Lsn, max_replicas: usize) -> DbResult<ReplicaId> {
        self.try_register_with_slot(start_lsn, None, max_replicas)
    }

    /// Register a new replica connection, optionally bound to a physical
    /// replication slot.
    pub fn try_register_with_slot(
        &self,
        start_lsn: Lsn,
        slot_name: Option<&str>,
        max_replicas: usize,
    ) -> DbResult<ReplicaId> {
        let mut inner = self.write_inner();
        if inner.replicas.len() >= max_replicas {
            return Err(DbError::internal(format!(
                "maximum number of WAL senders ({max_replicas}) reached"
            )));
        }
        let slot_name = if let Some(slot_name) = slot_name {
            validate_slot_name(slot_name)?;
            let slot = inner.slots.get_mut(slot_name).ok_or_else(|| {
                DbError::internal(format!(
                    "physical replication slot \"{slot_name}\" does not exist"
                ))
            })?;
            if slot.active_replica_id.is_some() {
                return Err(DbError::internal(format!(
                    "physical replication slot \"{slot_name}\" is already active"
                )));
            }
            if slot.slot.restart_lsn.is_none() {
                slot.slot.restart_lsn = Some(start_lsn);
                self.persist_slot(&slot.slot)?;
            }
            Some(slot_name.to_owned())
        } else {
            None
        };
        let id = ReplicaId::new(inner.next_id);
        inner.next_id = inner
            .next_id
            .checked_add(1)
            .ok_or_else(|| DbError::internal("replica registry exhausted replica identifiers"))?;
        let state = ReplicaState::new(id, start_lsn, slot_name.clone());
        if let Some(slot_name) = &slot_name {
            if let Some(slot) = inner.slots.get_mut(slot_name) {
                slot.active_replica_id = Some(id);
            }
        }
        inner.replicas.insert(id, state);
        Ok(id)
    }

    /// Remove a replica from the registry when it disconnects.
    pub fn unregister(&self, id: ReplicaId) {
        let mut inner = self.write_inner();
        let removed = inner.replicas.remove(&id);
        if let Some(slot_name) = removed.and_then(|state| state.slot_name) {
            if let Some(slot) = inner.slots.get_mut(&slot_name) {
                if slot.active_replica_id == Some(id) {
                    slot.active_replica_id = None;
                }
            }
        }
    }

    /// Update a replica's progress from a standby status update.
    pub fn update_progress(
        &self,
        id: ReplicaId,
        write_lsn: Lsn,
        flush_lsn: Lsn,
        apply_lsn: Lsn,
    ) -> DbResult<()> {
        let mut inner = self.write_inner();
        let slot_update = if let Some(state) = inner.replicas.get_mut(&id) {
            state.update_progress(write_lsn, flush_lsn, apply_lsn);
            state
                .slot_name
                .clone()
                .map(|slot_name| (slot_name, state.retention_lsn()))
        } else {
            None
        };
        if let Some((slot_name, next_restart_lsn)) = slot_update {
            if let Some(slot) = inner.slots.get_mut(&slot_name) {
                let should_persist = match slot.slot.restart_lsn {
                    Some(current) => next_restart_lsn > current,
                    None => true,
                };
                if should_persist {
                    slot.slot.restart_lsn = Some(next_restart_lsn);
                    self.persist_slot(&slot.slot)?;
                }
            }
        }
        drop(inner);
        self.sync_commit_cv.notify_all();
        Ok(())
    }

    /// Set the application name for a replica.
    pub fn set_application_name(&self, id: ReplicaId, name: String) {
        let mut inner = self.write_inner();
        if let Some(state) = inner.replicas.get_mut(&id) {
            state.application_name = Some(name);
        }
    }

    /// Return a snapshot of all replica states.
    pub fn snapshot(&self) -> Vec<ReplicaState> {
        let inner = self.read_inner();
        inner.replicas.values().cloned().collect()
    }

    /// Create and persist a new physical replication slot.
    pub fn create_physical_slot(
        &self,
        name: &str,
        current_lsn: Lsn,
        reserve_wal: bool,
        restart_tli: u64,
    ) -> DbResult<PhysicalReplicationSlot> {
        validate_slot_name(name)?;
        let mut inner = self.write_inner();
        if inner.slots.contains_key(name) {
            return Err(DbError::internal(format!(
                "physical replication slot \"{name}\" already exists"
            )));
        }
        let slot =
            PhysicalReplicationSlot::new(name, reserve_wal.then_some(current_lsn), restart_tli);
        self.persist_slot(&slot)?;
        inner.slots.insert(
            name.to_owned(),
            PersistentPhysicalReplicationSlot::new(slot.clone()),
        );
        Ok(slot)
    }

    /// Read a persisted physical replication slot.
    pub fn read_physical_slot(&self, name: &str) -> DbResult<Option<PhysicalReplicationSlot>> {
        validate_slot_name(name)?;
        let inner = self.read_inner();
        Ok(inner.slots.get(name).map(|slot| slot.slot.clone()))
    }

    /// Return the minimum flush LSN across all replicas, or `None` if no
    /// replicas are connected.
    ///
    /// This is used to determine the WAL retention boundary -- segments
    /// before this LSN are safe to remove if all replicas have flushed
    /// past it.
    pub fn min_flush_lsn(&self) -> Option<Lsn> {
        let inner = self.read_inner();
        inner.replicas.values().map(|s| s.flush_lsn).min()
    }

    /// Return the minimum retention LSN across all replicas, or `None` if no
    /// replicas are connected.
    pub fn min_retention_lsn(&self) -> Option<Lsn> {
        let inner = self.read_inner();
        let replica_min = inner
            .replicas
            .values()
            .map(ReplicaState::retention_lsn)
            .min();
        let slot_min = inner
            .slots
            .values()
            .filter_map(|slot| slot.slot.restart_lsn)
            .min();
        match (replica_min, slot_min) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (Some(left), None) => Some(left),
            (None, Some(right)) => Some(right),
            (None, None) => None,
        }
    }

    /// Return the number of currently connected replicas.
    pub fn count(&self) -> usize {
        let inner = self.read_inner();
        inner.replicas.len()
    }

    /// Returns `true` if any connected replica has `flush_lsn >= target_lsn`.
    pub fn any_replica_flushed_to(&self, target_lsn: Lsn) -> bool {
        let inner = self.read_inner();
        inner.replicas.values().any(|r| r.flush_lsn >= target_lsn)
    }

    /// Block until at least one connected replica has `flush_lsn >= target_lsn`,
    /// or the timeout expires.
    ///
    /// Returns `true` if the condition was met, `false` on timeout.
    /// Used by the synchronous commit path to wait for replica confirmation
    /// before acknowledging a commit to the client.
    pub fn wait_for_any_replica_flush(
        &self,
        target_lsn: Lsn,
        timeout: std::time::Duration,
    ) -> bool {
        if self.any_replica_flushed_to(target_lsn) {
            return true;
        }

        let guard = self
            .sync_commit_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let deadline = sync_commit_deadline(timeout);
        let mut guard = guard;
        loop {
            if self.any_replica_flushed_to(target_lsn) {
                return true;
            }
            if sync_commit_deadline_expired(deadline) {
                return self.any_replica_flushed_to(target_lsn);
            }
            let remaining = sync_commit_wait_remaining(deadline);
            let wait_result = self
                .sync_commit_cv
                .wait_timeout(guard, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard = wait_result.0;
            if wait_result.1.timed_out() && sync_commit_deadline_expired(deadline) {
                return self.any_replica_flushed_to(target_lsn);
            }
        }
    }

    /// Count how many connected replicas have `flush_lsn >= target_lsn`.
    pub fn replicas_flushed_count(&self, target_lsn: Lsn) -> usize {
        let inner = self.read_inner();
        inner
            .replicas
            .values()
            .filter(|r| r.flush_lsn >= target_lsn)
            .count()
    }

    /// Block until at least `required_count` replicas have
    /// `flush_lsn >= target_lsn`, or the timeout expires.
    ///
    /// Returns `true` if the condition was met, `false` on timeout.
    /// Used by the write-concern commit path to implement quorum,
    /// majority, and all-replica consistency levels.
    pub fn wait_for_write_concern(
        &self,
        target_lsn: Lsn,
        required_count: usize,
        timeout: std::time::Duration,
    ) -> bool {
        if required_count == 0 {
            return true;
        }
        if self.replicas_flushed_count(target_lsn) >= required_count {
            return true;
        }

        let guard = self
            .sync_commit_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let deadline = sync_commit_deadline(timeout);
        let mut guard = guard;
        loop {
            if self.replicas_flushed_count(target_lsn) >= required_count {
                return true;
            }
            if sync_commit_deadline_expired(deadline) {
                return self.replicas_flushed_count(target_lsn) >= required_count;
            }
            let remaining = sync_commit_wait_remaining(deadline);
            let wait_result = self
                .sync_commit_cv
                .wait_timeout(guard, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard = wait_result.0;
            if wait_result.1.timed_out() && sync_commit_deadline_expired(deadline) {
                return self.replicas_flushed_count(target_lsn) >= required_count;
            }
        }
    }

    fn persist_slot(&self, slot: &PhysicalReplicationSlot) -> DbResult<()> {
        let Some(slot_dir) = &self.slot_dir else {
            return Ok(());
        };
        let path = slot_metadata_path(slot_dir, &slot.name);
        write_slot_file_atomically(&path, slot)
    }
}

fn sync_commit_deadline(timeout: Duration) -> Option<Instant> {
    Instant::now().checked_add(timeout)
}

fn sync_commit_wait_remaining(deadline: Option<Instant>) -> Duration {
    match deadline {
        Some(deadline) => deadline
            .saturating_duration_since(Instant::now())
            .min(MAX_SYNC_COMMIT_WAIT_CHUNK),
        None => MAX_SYNC_COMMIT_WAIT_CHUNK,
    }
}

fn sync_commit_deadline_expired(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| deadline.saturating_duration_since(Instant::now()).is_zero())
}

impl Default for ReplicaRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// WAL Sender (primary side)
// ---------------------------------------------------------------------------

/// Reads WAL entries starting from a given LSN and produces batched
/// `WalData` messages for streaming to a replica.
///
/// The sender does not own a network connection -- it produces
/// `ReplicationMessage` values that the pgwire replication handler sends
/// over the wire.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LsnStepMode {
    Logical,
    ByteOffset,
}

struct ReceivedWalBatch {
    first_lsn: Lsn,
    last_lsn: Lsn,
    entries: Vec<crate::record::WalEntry>,
}

fn advance_lsn_checked(lsn: Lsn, step: u64, context: &str) -> DbResult<Lsn> {
    lsn.checked_advance(step).ok_or_else(|| {
        DbError::internal(format!(
            "WAL replication LSN overflow in {context}: {} + {}",
            lsn.get(),
            step
        ))
    })
}

pub struct WalSender {
    wal_dir: PathBuf,
    /// The next LSN to send.
    next_send_lsn: Lsn,
    /// The replica being served (for logging/metrics).
    replica_id: ReplicaId,
    /// Compression mode used when encoding outgoing WAL entries.
    wal_compression: WalCompression,
    lsn_step_mode: Option<LsnStepMode>,
    last_sent_lsn: Option<Lsn>,
    last_sent_entry_bytes: Option<u64>,
    reader: Option<WalReader>,
}

impl WalSender {
    /// Create a new WAL sender that will start streaming from `start_lsn`.
    pub fn new(wal_dir: PathBuf, start_lsn: Lsn, replica_id: ReplicaId) -> Self {
        Self::new_with_compression(wal_dir, start_lsn, replica_id, WalCompression::None)
    }

    /// Create a new WAL sender with an explicit compression mode for outgoing
    /// WAL batches.
    pub fn new_with_compression(
        wal_dir: PathBuf,
        start_lsn: Lsn,
        replica_id: ReplicaId,
        wal_compression: WalCompression,
    ) -> Self {
        Self {
            wal_dir,
            next_send_lsn: start_lsn,
            replica_id,
            wal_compression,
            lsn_step_mode: None,
            last_sent_lsn: None,
            last_sent_entry_bytes: None,
            reader: None,
        }
    }

    /// Read the next batch of WAL entries and produce a `WalData` message.
    ///
    /// Returns `Ok(None)` when there are no new entries available (the
    /// replica is caught up to the current end of WAL).
    pub fn next_batch(&mut self) -> DbResult<Option<ReplicationMessage>> {
        if self.reader.is_none() {
            self.reader = Some(WalReader::open(self.wal_dir.clone(), self.next_send_lsn)?);
        }
        let reader = self
            .reader
            .as_mut()
            .ok_or_else(|| DbError::internal("WAL sender reader initialization failed"))?;

        let mut entries_data = Vec::new();
        let mut first_lsn = Lsn::ZERO;
        let mut last_lsn = Lsn::ZERO;
        let mut last_entry_bytes = 0u64;
        let mut count = 0usize;
        let mut reached_end_of_stream = false;

        let mut previous_lsn = self.last_sent_lsn;
        let mut previous_entry_bytes = self.last_sent_entry_bytes;
        let mut lsn_step_mode = self.lsn_step_mode;

        while count < MAX_ENTRIES_PER_BATCH && entries_data.len() < MAX_BATCH_BYTES {
            match reader.next_entry_with_len()? {
                Some((entry, entry_bytes)) => {
                    if first_lsn == Lsn::ZERO {
                        if let Some(prev_lsn) = previous_lsn {
                            if entry.prev_lsn != Lsn::ZERO && entry.prev_lsn != prev_lsn {
                                return Err(DbError::internal(format!(
                                    "requested WAL starting at {} is no longer retained; backward-chain mismatch at LSN {}",
                                    self.next_send_lsn.get(),
                                    entry.lsn.get()
                                )));
                            }

                            let prev_entry_bytes = previous_entry_bytes.unwrap_or(1);
                            let logical_expected =
                                advance_lsn_checked(prev_lsn, 1, "sender batch start")?;
                            let byte_expected = advance_lsn_checked(
                                prev_lsn,
                                prev_entry_bytes,
                                "sender batch start",
                            )?;
                            match lsn_step_mode {
                                Some(LsnStepMode::Logical) if entry.lsn != logical_expected => {
                                    return Err(DbError::internal(format!(
                                        "requested WAL starting at {} is no longer retained; earliest available LSN is {}",
                                        self.next_send_lsn.get(),
                                        entry.lsn.get()
                                    )));
                                }
                                Some(LsnStepMode::ByteOffset) if entry.lsn != byte_expected => {
                                    return Err(DbError::internal(format!(
                                        "requested WAL starting at {} is no longer retained; earliest available LSN is {}",
                                        self.next_send_lsn.get(),
                                        entry.lsn.get()
                                    )));
                                }
                                None => {
                                    if entry.lsn == logical_expected {
                                        lsn_step_mode = Some(LsnStepMode::Logical);
                                    } else if entry.lsn == byte_expected {
                                        lsn_step_mode = Some(LsnStepMode::ByteOffset);
                                    } else {
                                        return Err(DbError::internal(format!(
                                            "requested WAL starting at {} is no longer retained; earliest available LSN is {}",
                                            self.next_send_lsn.get(),
                                            entry.lsn.get()
                                        )));
                                    }
                                }
                                _ => {}
                            }
                        } else if entry.lsn > self.next_send_lsn {
                            return Err(DbError::internal(format!(
                                "requested WAL starting at {} is no longer retained; earliest available LSN is {}",
                                self.next_send_lsn.get(),
                                entry.lsn.get()
                            )));
                        }
                        first_lsn = entry.lsn;
                    } else if let Some(prev_lsn) = previous_lsn {
                        if entry.prev_lsn != Lsn::ZERO && entry.prev_lsn != prev_lsn {
                            return Err(DbError::internal(format!(
                                "WAL stream continuity error for replica {}: backward-chain mismatch expected prev_lsn {}, found {}",
                                self.replica_id,
                                prev_lsn.get(),
                                entry.prev_lsn.get()
                            )));
                        }

                        let prev_entry_bytes = previous_entry_bytes.unwrap_or(1);
                        let logical_expected =
                            advance_lsn_checked(prev_lsn, 1, "sender batch continuity")?;
                        let byte_expected = advance_lsn_checked(
                            prev_lsn,
                            prev_entry_bytes,
                            "sender batch continuity",
                        )?;
                        match lsn_step_mode {
                            Some(LsnStepMode::Logical) if entry.lsn != logical_expected => {
                                return Err(DbError::internal(format!(
                                    "WAL stream continuity error for replica {}: expected logical LSN {}, found {}",
                                    self.replica_id,
                                    logical_expected.get(),
                                    entry.lsn.get()
                                )));
                            }
                            Some(LsnStepMode::ByteOffset) if entry.lsn != byte_expected => {
                                return Err(DbError::internal(format!(
                                    "WAL stream continuity error for replica {}: expected byte-offset LSN {}, found {}",
                                    self.replica_id,
                                    byte_expected.get(),
                                    entry.lsn.get()
                                )));
                            }
                            None => {
                                if entry.lsn == logical_expected {
                                    lsn_step_mode = Some(LsnStepMode::Logical);
                                } else if entry.lsn == byte_expected {
                                    lsn_step_mode = Some(LsnStepMode::ByteOffset);
                                } else {
                                    return Err(DbError::internal(format!(
                                        "WAL stream continuity error for replica {}: expected LSN {} (logical) or {} (byte-offset), found {}",
                                        self.replica_id,
                                        logical_expected.get(),
                                        byte_expected.get(),
                                        entry.lsn.get()
                                    )));
                                }
                            }
                            _ => {}
                        }
                    }
                    let encoded =
                        codec::encode_entry_with_compression(&entry, self.wal_compression)?;
                    if !entries_data.is_empty()
                        && entries_data.len().saturating_add(encoded.len()) > MAX_BATCH_BYTES
                    {
                        break;
                    }
                    last_lsn = entry.lsn;
                    last_entry_bytes = entry_bytes;
                    previous_lsn = Some(entry.lsn);
                    previous_entry_bytes = Some(entry_bytes);
                    entries_data.extend_from_slice(&encoded);
                    count += 1;
                }
                None => {
                    reached_end_of_stream = true;
                    break;
                }
            }
        }

        if count == 0 {
            self.reader = None;
            return Ok(None);
        }

        self.last_sent_lsn = Some(last_lsn);
        self.last_sent_entry_bytes = Some(last_entry_bytes);
        self.lsn_step_mode = lsn_step_mode;

        self.next_send_lsn = match lsn_step_mode {
            Some(LsnStepMode::Logical) => advance_lsn_checked(last_lsn, 1, "sender next_send_lsn")?,
            Some(LsnStepMode::ByteOffset) => {
                advance_lsn_checked(last_lsn, last_entry_bytes, "sender next_send_lsn")?
            }
            None => advance_lsn_checked(last_lsn, 1, "sender next_send_lsn")?,
        };
        if reached_end_of_stream {
            self.reader = None;
        }

        Ok(Some(ReplicationMessage::WalData {
            start_lsn: first_lsn,
            end_lsn: last_lsn,
            data: entries_data,
        }))
    }

    /// Create a keepalive message with the current WAL end position.
    pub fn keepalive(&self, wal_end: Lsn, reply_requested: bool) -> ReplicationMessage {
        ReplicationMessage::Keepalive {
            wal_end,
            timestamp_us: current_timestamp_us(),
            reply_requested,
        }
    }

    /// Return the replica ID being served.
    pub fn replica_id(&self) -> ReplicaId {
        self.replica_id
    }

    /// Return the next LSN that will be sent.
    pub fn next_send_lsn(&self) -> Lsn {
        self.next_send_lsn
    }

    /// Return the highest LSN already sent to this replica.
    pub fn last_sent_lsn(&self) -> Option<Lsn> {
        self.last_sent_lsn
    }
}

impl fmt::Debug for WalSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WalSender")
            .field("replica_id", &self.replica_id)
            .field("next_send_lsn", &self.next_send_lsn)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// WAL Receiver (replica side)
// ---------------------------------------------------------------------------

/// Accepts encoded WAL entry batches from a primary and writes them into
/// the local WAL segment directory.
///
/// The receiver does not own a network connection -- it consumes
/// `ReplicationMessage::WalData` values provided by the pgwire layer and
/// manages a local `WalWriter`.
pub struct WalReceiver {
    config: WalConfig,
    writer: Mutex<WalWriter>,
    /// The last LSN written to local WAL storage.
    write_lsn: Mutex<Lsn>,
    /// The last LSN flushed (durable) to local WAL storage.
    flush_lsn: Mutex<Lsn>,
    /// The last LSN replayed into the storage engine.
    apply_lsn: Mutex<Lsn>,
    local_system_identifier: Option<u64>,
    local_timeline_id: RwLock<Option<u32>>,
    receive_start_lsn: Mutex<Option<Lsn>>,
    last_msg_send_time: Mutex<Option<OffsetDateTime>>,
    last_msg_receipt_time: Mutex<Option<OffsetDateTime>>,
    latest_end_lsn: Mutex<Option<Lsn>>,
    latest_end_time: Mutex<Option<OffsetDateTime>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WalReceiverStatusSnapshot {
    pub receive_start_lsn: Option<Lsn>,
    pub write_lsn: Lsn,
    pub flush_lsn: Lsn,
    pub apply_lsn: Lsn,
    pub last_msg_send_time: Option<OffsetDateTime>,
    pub last_msg_receipt_time: Option<OffsetDateTime>,
    pub latest_end_lsn: Option<Lsn>,
    pub latest_end_time: Option<OffsetDateTime>,
    pub local_system_identifier: Option<u64>,
    pub local_timeline_id: Option<u32>,
}

impl WalReceiver {
    /// Create a new WAL receiver that writes to the given WAL directory.
    pub fn open(config: WalConfig) -> DbResult<Self> {
        let writer = WalWriter::open(config.clone())?;
        let write_lsn = writer.last_lsn().unwrap_or(Lsn::ZERO);
        let local_identity = segment::resolve_cluster_identity_from_wal_dir(&config.dir);
        Ok(Self {
            config,
            writer: Mutex::new(writer),
            write_lsn: Mutex::new(write_lsn),
            flush_lsn: Mutex::new(write_lsn),
            apply_lsn: Mutex::new(Lsn::ZERO),
            local_system_identifier: local_identity.system_identifier,
            local_timeline_id: RwLock::new(local_identity.timeline_id),
            receive_start_lsn: Mutex::new(None),
            last_msg_send_time: Mutex::new(None),
            last_msg_receipt_time: Mutex::new(None),
            latest_end_lsn: Mutex::new(None),
            latest_end_time: Mutex::new(None),
        })
    }

    /// Validate that the upstream primary identity matches the local replica
    /// metadata discovered next to the WAL directory.
    pub fn validate_upstream_identity(
        &self,
        system_identifier: u64,
        timeline_id: u32,
    ) -> DbResult<()> {
        if let Some(local_system_identifier) = self.local_system_identifier {
            if local_system_identifier != system_identifier {
                return Err(DbError::internal(format!(
                    "replication upstream system identifier mismatch: upstream has {}, local replica expects {}",
                    system_identifier, local_system_identifier
                )));
            }
        }
        let local_timeline_id = self
            .local_timeline_id
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(local_timeline_id) = *local_timeline_id {
            if local_timeline_id != timeline_id {
                return Err(DbError::internal(format!(
                    "replication upstream timeline mismatch: upstream has {}, local replica expects {}",
                    timeline_id, local_timeline_id
                )));
            }
        }
        Ok(())
    }

    /// Return the current locally-recorded timeline, if any.
    pub fn local_timeline(&self) -> Option<u32> {
        *self
            .local_timeline_id
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Atomically advance the replica's local timeline metadata to
    /// `timeline_id` and persist it next to the WAL directory. Only allowed
    /// if there is no current timeline or the current timeline is strictly
    /// smaller -- timelines never go backward in PG's replication model.
    pub fn set_local_timeline(&self, timeline_id: u32) -> DbResult<()> {
        if timeline_id == 0 {
            return Err(DbError::internal(
                "replication upstream timeline must be > 0",
            ));
        }
        let mut guard = self
            .local_timeline_id
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(current) = *guard {
            if current == timeline_id {
                return Ok(());
            }
            if current > timeline_id {
                return Err(DbError::internal(format!(
                    "refusing to roll back local timeline: current {current}, requested {timeline_id}"
                )));
            }
        }
        persist_local_timeline(&self.config.dir, timeline_id)?;
        *guard = Some(timeline_id);
        Ok(())
    }

    /// Process a full replication message on the replica side.
    ///
    /// Only `WalData` frames are accepted here. The advertised `start_lsn`
    /// and `end_lsn` bounds are validated against the decoded payload before
    /// any local WAL write is attempted.
    pub fn receive_message(&self, message: &ReplicationMessage) -> DbResult<Lsn> {
        match message {
            ReplicationMessage::WalData {
                start_lsn,
                end_lsn,
                data,
            } => {
                let mut writer = self.writer.lock().map_err(|e| {
                    DbError::internal(format!("WAL receiver writer lock poisoned: {e}"))
                })?;
                let mut write_lsn = self.write_lsn.lock().map_err(|e| {
                    DbError::internal(format!("WAL receiver write_lsn lock poisoned: {e}"))
                })?;
                let expected_start_lsn = writer.next_lsn();
                let batch = Self::decode_incoming_wal_batch(data, *write_lsn, expected_start_lsn)?;
                if batch.first_lsn != *start_lsn {
                    return Err(DbError::internal(format!(
                        "WAL receiver batch start_lsn mismatch: message says {}, payload starts at {}",
                        start_lsn.get(),
                        batch.first_lsn.get()
                    )));
                }
                if batch.last_lsn != *end_lsn {
                    return Err(DbError::internal(format!(
                        "WAL receiver batch end_lsn mismatch: message says {}, payload ends at {}",
                        end_lsn.get(),
                        batch.last_lsn.get()
                    )));
                }
                self.note_received_batch(batch.first_lsn, batch.last_lsn)?;
                self.apply_received_wal_batch(&mut writer, &mut write_lsn, batch)
            }
            ReplicationMessage::Keepalive {
                wal_end,
                timestamp_us,
                ..
            } => {
                self.note_keepalive(*wal_end, *timestamp_us)?;
                Ok(self.write_lsn())
            }
            _ => Err(DbError::internal(
                "WAL receiver expected WalData or Keepalive replication message",
            )),
        }
    }

    /// Process a `WalData` message: decode entries and append them to the
    /// local WAL. Returns the LSN of the last entry written.
    pub fn receive_wal_data(&self, data: &[u8]) -> DbResult<Lsn> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| DbError::internal(format!("WAL receiver writer lock poisoned: {e}")))?;
        let mut write_lsn = self
            .write_lsn
            .lock()
            .map_err(|e| DbError::internal(format!("WAL receiver write_lsn lock poisoned: {e}")))?;
        let expected_start_lsn = writer.next_lsn();
        let batch = Self::decode_incoming_wal_batch(data, *write_lsn, expected_start_lsn)?;
        self.note_received_batch(batch.first_lsn, batch.last_lsn)?;
        self.apply_received_wal_batch(&mut writer, &mut write_lsn, batch)
    }

    /// Flush the local WAL to durable storage.
    pub fn flush_durable(&self) -> DbResult<Lsn> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| DbError::internal(format!("WAL receiver writer lock poisoned: {e}")))?;
        writer.flush_durable()?;

        let write_lsn = self
            .write_lsn
            .lock()
            .map_err(|e| DbError::internal(format!("WAL receiver write_lsn lock poisoned: {e}")))?;
        let mut flush_lsn = self
            .flush_lsn
            .lock()
            .map_err(|e| DbError::internal(format!("WAL receiver flush_lsn lock poisoned: {e}")))?;
        *flush_lsn = *write_lsn;
        Ok(*flush_lsn)
    }

    /// Record that WAL entries up to `lsn` have been replayed into the
    /// storage engine.
    pub fn set_apply_lsn(&self, lsn: Lsn) {
        let flush_lsn = *self
            .flush_lsn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut apply_lsn = self
            .apply_lsn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *apply_lsn > flush_lsn {
            *apply_lsn = flush_lsn;
        }
        let bounded_lsn = lsn.min(flush_lsn);
        if bounded_lsn > *apply_lsn {
            *apply_lsn = bounded_lsn;
        }
    }

    /// Build a standby status update message reflecting current progress.
    pub fn status_update(&self) -> ReplicationMessage {
        let write_lsn = *self
            .write_lsn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let flush_lsn = *self
            .flush_lsn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let apply_lsn = *self
            .apply_lsn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        ReplicationMessage::StandbyStatusUpdate {
            write_lsn,
            flush_lsn,
            apply_lsn,
        }
    }

    /// Return the last LSN written to local WAL.
    pub fn write_lsn(&self) -> Lsn {
        *self
            .write_lsn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Return the last LSN flushed durably.
    pub fn flush_lsn(&self) -> Lsn {
        *self
            .flush_lsn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Return the last LSN replayed into the storage engine.
    pub fn apply_lsn(&self) -> Lsn {
        *self
            .apply_lsn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn status_snapshot(&self) -> WalReceiverStatusSnapshot {
        WalReceiverStatusSnapshot {
            receive_start_lsn: *self
                .receive_start_lsn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            write_lsn: self.write_lsn(),
            flush_lsn: self.flush_lsn(),
            apply_lsn: self.apply_lsn(),
            last_msg_send_time: *self
                .last_msg_send_time
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            last_msg_receipt_time: *self
                .last_msg_receipt_time
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            latest_end_lsn: *self
                .latest_end_lsn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            latest_end_time: *self
                .latest_end_time
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            local_system_identifier: self.local_system_identifier,
            local_timeline_id: self.local_timeline(),
        }
    }

    fn note_received_batch(&self, start_lsn: Lsn, end_lsn: Lsn) -> DbResult<()> {
        let now = OffsetDateTime::now_utc();
        {
            let mut receive_start_lsn = self.receive_start_lsn.lock().map_err(|e| {
                DbError::internal(format!("WAL receiver receive_start_lsn lock poisoned: {e}"))
            })?;
            if receive_start_lsn.is_none() {
                *receive_start_lsn = Some(start_lsn);
            }
        }
        *self.last_msg_receipt_time.lock().map_err(|e| {
            DbError::internal(format!(
                "WAL receiver last_msg_receipt_time lock poisoned: {e}"
            ))
        })? = Some(now);
        *self.latest_end_lsn.lock().map_err(|e| {
            DbError::internal(format!("WAL receiver latest_end_lsn lock poisoned: {e}"))
        })? = Some(end_lsn);
        *self.latest_end_time.lock().map_err(|e| {
            DbError::internal(format!("WAL receiver latest_end_time lock poisoned: {e}"))
        })? = Some(now);
        Ok(())
    }

    fn note_keepalive(&self, wal_end: Lsn, timestamp_us: u64) -> DbResult<()> {
        let receipt_time = OffsetDateTime::now_utc();
        *self.last_msg_send_time.lock().map_err(|e| {
            DbError::internal(format!(
                "WAL receiver last_msg_send_time lock poisoned: {e}"
            ))
        })? = offset_datetime_from_unix_micros(timestamp_us);
        *self.last_msg_receipt_time.lock().map_err(|e| {
            DbError::internal(format!(
                "WAL receiver last_msg_receipt_time lock poisoned: {e}"
            ))
        })? = Some(receipt_time);
        *self.latest_end_lsn.lock().map_err(|e| {
            DbError::internal(format!("WAL receiver latest_end_lsn lock poisoned: {e}"))
        })? = Some(wal_end);
        *self.latest_end_time.lock().map_err(|e| {
            DbError::internal(format!("WAL receiver latest_end_time lock poisoned: {e}"))
        })? = Some(receipt_time);
        Ok(())
    }

    fn resync_after_failed_receive(
        &self,
        writer: &mut WalWriter,
        write_lsn: &mut Lsn,
        flush_lsn: &mut Lsn,
    ) -> DbResult<()> {
        *writer = WalWriter::open(self.config.clone())?;
        *write_lsn = writer.last_lsn().unwrap_or(Lsn::ZERO);
        if *flush_lsn > *write_lsn {
            *flush_lsn = *write_lsn;
        }
        let mut apply_lsn = self
            .apply_lsn
            .lock()
            .map_err(|e| DbError::internal(format!("WAL receiver apply_lsn lock poisoned: {e}")))?;
        if *apply_lsn > *flush_lsn {
            *apply_lsn = *flush_lsn;
        }
        Ok(())
    }

    fn decode_incoming_wal_batch(
        data: &[u8],
        write_lsn: Lsn,
        expected_start_lsn: Lsn,
    ) -> DbResult<ReceivedWalBatch> {
        let mut offset = 0;
        let mut entries = Vec::new();
        let mut previous_lsn: Option<Lsn> = None;
        let mut previous_entry_bytes: Option<u64> = None;
        let mut step_mode: Option<LsnStepMode> = None;

        while offset < data.len() {
            let (entry, consumed) = codec::decode_entry(&data[offset..])?;
            let consumed_u64 = usize_to_u64_saturating(consumed);

            if let Some(previous_lsn) = previous_lsn {
                if entry.prev_lsn != Lsn::ZERO && entry.prev_lsn != previous_lsn {
                    return Err(DbError::internal(format!(
                        "WAL receiver backward-chain mismatch: expected prev_lsn {}, got {}",
                        previous_lsn.get(),
                        entry.prev_lsn.get()
                    )));
                }

                let previous_entry_bytes = previous_entry_bytes.unwrap_or(1);
                let logical_expected = previous_lsn.checked_advance(1).ok_or_else(|| {
                    DbError::internal(format!(
                        "WAL receiver LSN overflow after {}",
                        previous_lsn.get()
                    ))
                })?;
                let byte_expected = previous_lsn
                    .checked_advance(previous_entry_bytes)
                    .ok_or_else(|| {
                        DbError::internal(format!(
                            "WAL receiver LSN overflow after {}",
                            previous_lsn.get()
                        ))
                    })?;
                match step_mode {
                    Some(LsnStepMode::Logical) => {
                        if entry.lsn != logical_expected {
                            return Err(DbError::internal(format!(
                                "WAL receiver out-of-order LSN: expected logical {}, got {}",
                                logical_expected.get(),
                                entry.lsn.get()
                            )));
                        }
                    }
                    Some(LsnStepMode::ByteOffset) => {
                        if entry.lsn != byte_expected {
                            return Err(DbError::internal(format!(
                                "WAL receiver out-of-order LSN: expected byte-offset {}, got {}",
                                byte_expected.get(),
                                entry.lsn.get()
                            )));
                        }
                    }
                    None => {
                        if entry.lsn == logical_expected {
                            step_mode = Some(LsnStepMode::Logical);
                        } else if entry.lsn == byte_expected {
                            step_mode = Some(LsnStepMode::ByteOffset);
                        } else {
                            return Err(DbError::internal(format!(
                                "WAL receiver out-of-order LSN: expected {} (logical) or {} (byte-offset), got {}",
                                logical_expected.get(),
                                byte_expected.get(),
                                entry.lsn.get()
                            )));
                        }
                    }
                }
            } else {
                if entry.prev_lsn != Lsn::ZERO && entry.prev_lsn != write_lsn {
                    return Err(DbError::internal(format!(
                        "WAL receiver backward-chain mismatch at batch start: expected prev_lsn {}, got {}",
                        write_lsn.get(),
                        entry.prev_lsn.get()
                    )));
                }
                if entry.lsn != expected_start_lsn {
                    return Err(DbError::internal(format!(
                        "WAL receiver out-of-order LSN: expected {}, got {}",
                        expected_start_lsn.get(),
                        entry.lsn.get()
                    )));
                }
            }

            entries.push(entry);
            offset += consumed;
            previous_lsn = entries.last().map(|entry| entry.lsn);
            previous_entry_bytes = Some(consumed_u64);
        }

        let Some(first_lsn) = entries.first().map(|entry| entry.lsn) else {
            return Err(DbError::internal("WAL receiver got empty WalData payload"));
        };
        let Some(last_lsn) = entries.last().map(|entry| entry.lsn) else {
            return Err(DbError::internal("WAL receiver got empty WalData payload"));
        };

        Ok(ReceivedWalBatch {
            first_lsn,
            last_lsn,
            entries,
        })
    }

    fn apply_received_wal_batch(
        &self,
        writer: &mut WalWriter,
        write_lsn: &mut Lsn,
        batch: ReceivedWalBatch,
    ) -> DbResult<Lsn> {
        let apply_result = (|| -> DbResult<()> {
            for entry in &batch.entries {
                let assigned = writer.append(&entry.record)?;
                if assigned != entry.lsn {
                    return Err(DbError::internal(format!(
                        "WAL receiver local LSN mismatch: expected {}, assigned {}",
                        entry.lsn.get(),
                        assigned.get()
                    )));
                }
            }
            writer.flush()?;
            Ok(())
        })();

        if let Err(error) = apply_result {
            let mut flush_lsn = self.flush_lsn.lock().map_err(|e| {
                DbError::internal(format!("WAL receiver flush_lsn lock poisoned: {e}"))
            })?;
            if let Err(resync_error) =
                self.resync_after_failed_receive(writer, write_lsn, &mut flush_lsn)
            {
                return Err(DbError::internal(format!(
                    "WAL receiver apply failed and resync failed: {error}; {resync_error}"
                )));
            }
            return Err(error);
        }

        *write_lsn = batch.last_lsn;
        Ok(batch.last_lsn)
    }
}

impl fmt::Debug for WalReceiver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WalReceiver")
            .field("write_lsn", &self.write_lsn())
            .field("flush_lsn", &self.flush_lsn())
            .field("apply_lsn", &self.apply_lsn())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Replication configuration
// ---------------------------------------------------------------------------

/// Server replication role.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReplicationRole {
    /// This server is the primary and accepts writes.
    Primary,
    /// This server is a read-only replica receiving WAL from a primary.
    Replica,
    /// Replication is disabled (standalone mode -- the default).
    #[default]
    Standalone,
}

impl ReplicationRole {
    /// Returns `true` if this server should reject write operations.
    pub fn is_read_only(self) -> bool {
        matches!(self, Self::Replica)
    }

    /// Returns `true` if this server should accept replication connections.
    pub fn accepts_replication_connections(self) -> bool {
        matches!(self, Self::Primary)
    }

    /// Parse from a string (case-insensitive).
    pub fn from_str_value(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "primary" => Some(Self::Primary),
            "replica" | "standby" => Some(Self::Replica),
            "standalone" | "off" | "disabled" => Some(Self::Standalone),
            _ => None,
        }
    }

    /// Return the role as a lowercase string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Replica => "replica",
            Self::Standalone => "standalone",
        }
    }
}

/// Configuration for streaming replication.
#[derive(Clone, Debug)]
pub struct ReplicationConfig {
    /// The role of this server in the replication topology.
    pub role: ReplicationRole,
    /// Connection string to the primary server (used when role is `Replica`).
    /// Format: `host=<host> port=<port> dbname=<db> user=<user>`
    pub primary_conninfo: Option<String>,
    /// Maximum number of concurrent replication connections (used when role
    /// is `Primary`).
    pub max_wal_senders: u32,
    /// Minimum number of WAL segments to retain for replica catch-up.
    pub wal_keep_segments: u32,
    /// Interval in milliseconds between standby status updates sent by a
    /// replica to the primary.
    pub status_interval_ms: u64,
    /// Whether synchronous replication is required. When `true`, the primary
    /// waits for at least one replica to confirm flush before acknowledging
    /// a commit to the client.
    pub synchronous_commit: bool,
}

impl Default for ReplicationConfig {
    fn default() -> Self {
        Self {
            role: ReplicationRole::Standalone,
            primary_conninfo: None,
            max_wal_senders: 10,
            wal_keep_segments: 64,
            status_interval_ms: 10_000,
            synchronous_commit: false,
        }
    }
}

impl ReplicationConfig {
    /// Validate the configuration for consistency.
    pub fn validate(&self) -> DbResult<()> {
        if self.role == ReplicationRole::Replica && self.primary_conninfo.is_none() {
            return Err(DbError::internal(
                "replication role is 'replica' but primary_conninfo is not set",
            ));
        }
        if self.max_wal_senders == 0 && self.role == ReplicationRole::Primary {
            return Err(DbError::internal(
                "max_wal_senders must be > 0 when role is 'primary'",
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write a freshly-promoted timeline id next to a WAL directory. Searches
/// for an existing `replication/` sibling first (so a previously bootstrapped
/// replica keeps its layout); falls back to creating one next to the WAL
/// directory.
fn persist_local_timeline(wal_dir: &std::path::Path, timeline_id: u32) -> DbResult<()> {
    let replication_dir = wal_dir
        .parent()
        .map(|parent| parent.join("replication"))
        .unwrap_or_else(|| wal_dir.join("replication"));
    fs::create_dir_all(&replication_dir).map_err(|error| {
        DbError::internal(format!(
            "failed to create replication metadata dir {}: {error}",
            replication_dir.display()
        ))
    })?;
    let path = replication_dir.join("timeline");
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, format!("{timeline_id}\n")).map_err(|error| {
        DbError::internal(format!(
            "failed to write timeline temp file {}: {error}",
            tmp.display()
        ))
    })?;
    fs::rename(&tmp, &path).map_err(|error| {
        DbError::internal(format!(
            "failed to publish timeline metadata {}: {error}",
            path.display()
        ))
    })?;
    Ok(())
}

/// Return a monotonic timestamp in microseconds.
fn current_timestamp_us() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
}

fn offset_datetime_from_unix_micros(timestamp_us: u64) -> Option<OffsetDateTime> {
    let seconds = timestamp_us / 1_000_000;
    let micros = timestamp_us % 1_000_000;
    let nanos = micros.saturating_mul(1_000);
    let seconds = i64::try_from(seconds).ok()?;
    OffsetDateTime::from_unix_timestamp(seconds)
        .ok()
        .and_then(|dt| dt.replace_nanosecond(nanos as u32).ok())
}

#[cfg(test)]
mod tests;
