//! Stable contracts for the distributed AionDB split.
//!
//! This module is intentionally interface-only. It gives the frontend,
//! planner, shard engine, transaction coordinator, and fragment transport a
//! shared vocabulary without making the current single-process engine depend
//! on a concrete network, Raft, or storage implementation.

#![allow(clippy::doc_markdown, clippy::missing_errors_doc)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::Hash;
use std::sync::RwLock;
use std::time::Duration;

use aiondb_core::{DbResult, RelationId, TxnId};

use crate::DatabaseId;

#[derive(
    Clone, Debug, Eq, Hash, PartialEq, Ord, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct NodeId(String);

impl NodeId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn local() -> Self {
        Self::new("local")
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for NodeId {
    fn default() -> Self {
        Self::local()
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(
    Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct ShardId(u32);

impl ShardId {
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for ShardId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "shard-{}", self.0)
    }
}

#[derive(
    Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct QueryId(u128);

impl QueryId {
    #[must_use]
    pub const fn new(value: u128) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u128 {
        self.0
    }
}

#[derive(
    Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct FragmentId(u64);

impl FragmentId {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    Hash,
    PartialEq,
    Ord,
    PartialOrd,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct CatalogVersion(u64);

impl CatalogVersion {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    Hash,
    PartialEq,
    Ord,
    PartialOrd,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct PlacementEpoch(u64);

impl PlacementEpoch {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    Hash,
    PartialEq,
    Ord,
    PartialOrd,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct SnapshotTimestamp(u64);

impl SnapshotTimestamp {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ReplicaRole {
    Leader,
    Follower,
    Learner,
}

impl ReplicaRole {
    #[must_use]
    pub const fn is_voting(self) -> bool {
        matches!(self, Self::Leader | Self::Follower)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NodeDescriptor {
    pub node_id: NodeId,
    pub rpc_endpoint: String,
    pub is_live: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ControlPlaneNodeSnapshot {
    pub node_id: NodeId,
    pub rpc_endpoint: String,
    pub is_live: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ControlPlaneSnapshot {
    pub catalog_version: CatalogVersion,
    pub placement_epoch: PlacementEpoch,
    pub total_nodes: usize,
    pub live_nodes: usize,
    pub total_shards: usize,
    pub nodes: Vec<ControlPlaneNodeSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ShardPlacement {
    pub shard_id: ShardId,
    pub node_id: NodeId,
    pub role: ReplicaRole,
    pub lease_epoch: PlacementEpoch,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ShardDescriptor {
    pub database_id: DatabaseId,
    pub table_id: RelationId,
    pub shard_id: ShardId,
    pub placements: Vec<ShardPlacement>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EpochLease {
    pub shard_id: ShardId,
    pub leader: NodeId,
    pub epoch: PlacementEpoch,
    pub expires_after: Option<Duration>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TxnScope {
    #[default]
    Local,
    SingleShard {
        shard_id: ShardId,
    },
    ReadOnlyMultiShard {
        snapshot_ts: SnapshotTimestamp,
        shard_ids: Vec<ShardId>,
    },
    MultiShardWrite {
        coordinator: NodeId,
        participant_shards: Vec<ShardId>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TxnParticipant {
    pub shard_id: ShardId,
    pub leader: NodeId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TxnDecision {
    Commit { commit_ts: SnapshotTimestamp },
    Abort,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TxnRecordStatus {
    Active,
    Prepared,
    Committed,
    Aborted,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TxnRecord {
    pub txn_id: TxnId,
    pub scope: TxnScope,
    pub status: TxnRecordStatus,
    pub participants: Vec<TxnParticipant>,
    pub decision: Option<TxnDecision>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FragmentRuntimeOptions {
    pub query_id: QueryId,
    pub fragment_id: FragmentId,
    pub catalog_version: CatalogVersion,
    pub placement_epoch: PlacementEpoch,
    pub txn_scope: TxnScope,
    pub timeout: Option<Duration>,
}

pub trait MetadataReader: Send + Sync + std::fmt::Debug {
    fn catalog_version(&self) -> DbResult<CatalogVersion>;
    fn placement_epoch(&self) -> DbResult<PlacementEpoch>;
    fn database_shards(&self, database_id: DatabaseId) -> DbResult<Vec<ShardDescriptor>>;
    fn table_shards(
        &self,
        database_id: DatabaseId,
        table_id: RelationId,
    ) -> DbResult<Vec<ShardDescriptor>>;
}

pub trait MetadataWriter: Send + Sync + std::fmt::Debug {
    fn upsert_shard(&self, shard: ShardDescriptor) -> DbResult<()>;
    fn remove_table_shards(&self, database_id: DatabaseId, table_id: RelationId) -> DbResult<()>;
    fn update_placement(
        &self,
        shard_id: ShardId,
        placements: Vec<ShardPlacement>,
    ) -> DbResult<PlacementEpoch>;
    fn update_table_shard_placement(
        &self,
        database_id: DatabaseId,
        table_id: RelationId,
        shard_id: ShardId,
        placements: Vec<ShardPlacement>,
    ) -> DbResult<PlacementEpoch>;
}

pub trait NodeMembership: Send + Sync + std::fmt::Debug {
    fn upsert_node(&self, node: NodeDescriptor) -> DbResult<PlacementEpoch>;
    fn mark_node_live(&self, node_id: &NodeId, is_live: bool) -> DbResult<PlacementEpoch>;
    fn node(&self, node_id: &NodeId) -> DbResult<Option<NodeDescriptor>>;
    fn nodes(&self) -> DbResult<Vec<NodeDescriptor>>;
    fn live_nodes(&self) -> DbResult<Vec<NodeDescriptor>>;
}

pub trait ShardResolver: Send + Sync + std::fmt::Debug {
    fn resolve_read_shards(
        &self,
        database_id: DatabaseId,
        table_id: RelationId,
    ) -> DbResult<Vec<ShardPlacement>>;

    fn resolve_write_shard(
        &self,
        database_id: DatabaseId,
        table_id: RelationId,
        shard_key: &[u8],
    ) -> DbResult<ShardPlacement>;
}

pub trait DataPlaneLocalExecutor: Send + Sync + std::fmt::Debug {
    type Command: Send + Sync;
    type Output: Send + Sync;

    fn execute_local(
        &self,
        shard_id: ShardId,
        command: &Self::Command,
        scope: &TxnScope,
    ) -> DbResult<Self::Output>;
}

pub trait TxnCoordinator: Send + Sync + std::fmt::Debug {
    fn begin(&self, scope: TxnScope) -> DbResult<TxnId>;
    fn allocate_read_snapshot(&self, shard_ids: Vec<ShardId>) -> DbResult<TxnScope>;
    fn prepare(&self, txn_id: TxnId, participants: &[TxnParticipant]) -> DbResult<()>;
    fn decide(&self, txn_id: TxnId, decision: TxnDecision) -> DbResult<()>;
    fn abort(&self, txn_id: TxnId) -> DbResult<()>;
}

pub trait RemoteExecutor: Send + Sync + std::fmt::Debug {
    type FragmentPlan: Send + Sync;
    type FragmentResult: Send + Sync;

    fn execute_remote(
        &self,
        target: &NodeId,
        plan: &Self::FragmentPlan,
        options: &FragmentRuntimeOptions,
    ) -> DbResult<Self::FragmentResult>;

    fn cancel(&self, target: &NodeId, query_id: QueryId, fragment_id: FragmentId) -> DbResult<()>;
}

pub trait ReplicaController: Send + Sync + std::fmt::Debug {
    fn leader_for(&self, shard_id: ShardId) -> DbResult<Option<NodeId>>;
    fn transfer_leadership(&self, shard_id: ShardId, target: NodeId) -> DbResult<PlacementEpoch>;
    fn current_lease(&self, shard_id: ShardId) -> DbResult<Option<EpochLease>>;
}

pub trait ControlPlane:
    MetadataReader
    + MetadataWriter
    + NodeMembership
    + ShardResolver
    + ReplicaController
    + Send
    + Sync
    + std::fmt::Debug
{
}

impl<T> ControlPlane for T where
    T: MetadataReader
        + MetadataWriter
        + NodeMembership
        + ShardResolver
        + ReplicaController
        + Send
        + Sync
        + std::fmt::Debug
{
}

#[derive(Debug, Default)]
pub struct InMemoryControlPlane {
    inner: RwLock<InMemoryControlPlaneState>,
}

#[derive(Debug, Default)]
pub struct InMemoryTxnCoordinator {
    inner: RwLock<InMemoryTxnCoordinatorState>,
}

#[derive(Debug, Default)]
struct InMemoryControlPlaneState {
    catalog_version: CatalogVersion,
    placement_epoch: PlacementEpoch,
    nodes: BTreeMap<NodeId, NodeDescriptor>,
    shards: BTreeMap<ShardMapKey, ShardDescriptor>,
    shards_by_table: HashMap<(DatabaseId, RelationId), Vec<ShardId>>,
}

type ShardMapKey = (DatabaseId, RelationId, ShardId);

#[derive(Debug)]
struct InMemoryTxnCoordinatorState {
    next_txn_id: u64,
    next_snapshot_ts: u64,
    records: BTreeMap<TxnId, TxnRecord>,
}

impl Default for InMemoryTxnCoordinatorState {
    fn default() -> Self {
        Self {
            next_txn_id: 1,
            next_snapshot_ts: 1,
            records: BTreeMap::new(),
        }
    }
}

impl InMemoryControlPlane {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> DbResult<ControlPlaneSnapshot> {
        let guard = self.lock_read()?;
        let nodes: Vec<_> = guard
            .nodes
            .values()
            .map(|node| ControlPlaneNodeSnapshot {
                node_id: node.node_id.clone(),
                rpc_endpoint: node.rpc_endpoint.clone(),
                is_live: node.is_live,
            })
            .collect();
        Ok(ControlPlaneSnapshot {
            catalog_version: guard.catalog_version,
            placement_epoch: guard.placement_epoch,
            total_nodes: nodes.len(),
            live_nodes: nodes.iter().filter(|node| node.is_live).count(),
            total_shards: guard.shards.len(),
            nodes,
        })
    }

    fn lock_read(&self) -> DbResult<std::sync::RwLockReadGuard<'_, InMemoryControlPlaneState>> {
        self.inner
            .read()
            .map_err(|_| aiondb_core::DbError::internal("distributed control plane poisoned"))
    }

    fn lock_write(&self) -> DbResult<std::sync::RwLockWriteGuard<'_, InMemoryControlPlaneState>> {
        self.inner
            .write()
            .map_err(|_| aiondb_core::DbError::internal("distributed control plane poisoned"))
    }
}

impl InMemoryTxnCoordinator {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, txn_id: TxnId) -> DbResult<Option<TxnRecord>> {
        Ok(self.lock_read()?.records.get(&txn_id).cloned())
    }

    fn lock_read(&self) -> DbResult<std::sync::RwLockReadGuard<'_, InMemoryTxnCoordinatorState>> {
        self.inner
            .read()
            .map_err(|_| aiondb_core::DbError::internal("transaction coordinator poisoned"))
    }

    fn lock_write(&self) -> DbResult<std::sync::RwLockWriteGuard<'_, InMemoryTxnCoordinatorState>> {
        self.inner
            .write()
            .map_err(|_| aiondb_core::DbError::internal("transaction coordinator poisoned"))
    }
}

fn next_catalog_version(version: CatalogVersion) -> CatalogVersion {
    CatalogVersion::new(version.get().saturating_add(1))
}

fn next_placement_epoch(epoch: PlacementEpoch) -> PlacementEpoch {
    PlacementEpoch::new(epoch.get().saturating_add(1))
}

/// Stable, cross-process hash for shard routing. `DefaultHasher`'s SipHash
/// uses a per-process random seed so the same `shard_key` would route to
/// (64-bit) which is deterministic, has no seed, and matches in any
/// release of the binary regardless of compiler/std version.
fn shard_key_hash(shard_key: &[u8]) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET_BASIS;
    for &byte in shard_key {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn leader_placement(shard: &ShardDescriptor) -> Option<ShardPlacement> {
    shard
        .placements
        .iter()
        .find(|placement| placement.role == ReplicaRole::Leader)
        .cloned()
}

fn leader_placement_required(shard: &ShardDescriptor) -> DbResult<ShardPlacement> {
    leader_placement(shard).ok_or_else(|| {
        aiondb_core::DbError::internal(format!("shard {} has no leader placement", shard.shard_id))
    })
}

fn shard_map_key(shard: &ShardDescriptor) -> ShardMapKey {
    (shard.database_id, shard.table_id, shard.shard_id)
}

fn shard_map_key_for(
    database_id: DatabaseId,
    table_id: RelationId,
    shard_id: ShardId,
) -> ShardMapKey {
    (database_id, table_id, shard_id)
}

fn unique_shard_map_key_for_id(
    state: &InMemoryControlPlaneState,
    shard_id: ShardId,
) -> DbResult<Option<ShardMapKey>> {
    let mut matches = state
        .shards
        .keys()
        .copied()
        .filter(|(_, _, existing_shard_id)| *existing_shard_id == shard_id);
    let Some(first) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Err(aiondb_core::DbError::internal(format!(
            "shard {shard_id} is ambiguous across multiple tables"
        )));
    }
    Ok(Some(first))
}

fn validate_node_descriptor(node: &NodeDescriptor) -> DbResult<()> {
    if node.node_id.as_str().trim().is_empty() {
        return Err(aiondb_core::DbError::internal(
            "node descriptor has an empty node id",
        ));
    }
    if node.rpc_endpoint.trim().is_empty() {
        return Err(aiondb_core::DbError::internal(format!(
            "node {} has an empty rpc endpoint",
            node.node_id
        )));
    }
    Ok(())
}

fn validate_shard_descriptor(shard: &ShardDescriptor) -> DbResult<()> {
    validate_shard_placements(shard.shard_id, &shard.placements)
}

fn validate_txn_scope(scope: &TxnScope) -> DbResult<()> {
    match scope {
        TxnScope::Local | TxnScope::SingleShard { .. } => Ok(()),
        TxnScope::ReadOnlyMultiShard { shard_ids, .. } => {
            validate_unique_shards("read-only transaction", shard_ids)
        }
        TxnScope::MultiShardWrite {
            participant_shards, ..
        } => validate_unique_shards("multi-shard write transaction", participant_shards),
    }
}

fn validate_unique_shards(context: &str, shard_ids: &[ShardId]) -> DbResult<()> {
    if shard_ids.is_empty() {
        return Err(aiondb_core::DbError::internal(format!(
            "{context} has no participant shards"
        )));
    }
    let mut seen = HashSet::new();
    for shard_id in shard_ids {
        if !seen.insert(*shard_id) {
            return Err(aiondb_core::DbError::internal(format!(
                "{context} has duplicate participant shard {shard_id}"
            )));
        }
    }
    Ok(())
}

fn validate_txn_participants(scope: &TxnScope, participants: &[TxnParticipant]) -> DbResult<()> {
    if participants.is_empty() {
        return Err(aiondb_core::DbError::internal(
            "transaction prepare has no participants",
        ));
    }

    let mut participant_shards = Vec::with_capacity(participants.len());
    let mut participant_leaders = HashSet::new();
    for participant in participants {
        if participant.leader.as_str().trim().is_empty() {
            return Err(aiondb_core::DbError::internal(format!(
                "participant for shard {} has an empty leader",
                participant.shard_id
            )));
        }
        if !participant_leaders.insert((participant.shard_id, participant.leader.clone())) {
            return Err(aiondb_core::DbError::internal(format!(
                "duplicate participant for shard {} on node {}",
                participant.shard_id, participant.leader
            )));
        }
        participant_shards.push(participant.shard_id);
    }
    validate_unique_shards("transaction prepare", &participant_shards)?;

    match scope {
        TxnScope::Local => Err(aiondb_core::DbError::internal(
            "local transaction cannot prepare distributed participants",
        )),
        TxnScope::SingleShard { shard_id } => {
            if participant_shards == [*shard_id] {
                Ok(())
            } else {
                Err(aiondb_core::DbError::internal(format!(
                    "single-shard transaction for {shard_id} cannot prepare participants {participant_shards:?}"
                )))
            }
        }
        TxnScope::ReadOnlyMultiShard { shard_ids, .. }
        | TxnScope::MultiShardWrite {
            participant_shards: shard_ids,
            ..
        } => {
            let mut expected = shard_ids.clone();
            let mut actual = participant_shards;
            expected.sort_unstable();
            actual.sort_unstable();
            if expected == actual {
                Ok(())
            } else {
                Err(aiondb_core::DbError::internal(format!(
                    "transaction participants {actual:?} do not match scope shards {expected:?}"
                )))
            }
        }
    }
}

pub fn validate_txn_scope_fragment_metadata(
    label: &str,
    scope: Option<&TxnScope>,
    shard_id: Option<u32>,
    snapshot_ts: Option<u64>,
) -> DbResult<()> {
    let Some(scope) = scope else {
        return Ok(());
    };
    match scope {
        TxnScope::Local => Ok(()),
        TxnScope::SingleShard {
            shard_id: scope_shard_id,
        } => {
            if let Some(metadata_shard_id) = shard_id {
                if metadata_shard_id != scope_shard_id.get() {
                    return Err(aiondb_core::DbError::internal(format!(
                        "{label} shard_id {} does not match single-shard txn scope {}",
                        metadata_shard_id,
                        scope_shard_id.get()
                    )));
                }
            }
            Ok(())
        }
        TxnScope::ReadOnlyMultiShard {
            snapshot_ts: scope_snapshot_ts,
            shard_ids,
        } => {
            if let Some(metadata_snapshot_ts) = snapshot_ts {
                if metadata_snapshot_ts != scope_snapshot_ts.get() {
                    return Err(aiondb_core::DbError::internal(format!(
                        "{label} snapshot_ts {} does not match read snapshot {}",
                        metadata_snapshot_ts,
                        scope_snapshot_ts.get()
                    )));
                }
            }
            if let Some(metadata_shard_id) = shard_id {
                let shard_is_participant = shard_ids
                    .iter()
                    .any(|scope_shard_id| scope_shard_id.get() == metadata_shard_id);
                if !shard_is_participant {
                    return Err(aiondb_core::DbError::internal(format!(
                        "{label} shard_id {} is not part of read-only txn scope",
                        metadata_shard_id
                    )));
                }
            }
            Ok(())
        }
        TxnScope::MultiShardWrite {
            participant_shards, ..
        } => {
            if let Some(metadata_shard_id) = shard_id {
                let shard_is_participant = participant_shards
                    .iter()
                    .any(|scope_shard_id| scope_shard_id.get() == metadata_shard_id);
                if !shard_is_participant {
                    return Err(aiondb_core::DbError::internal(format!(
                        "{label} shard_id {} is not part of multi-shard write txn scope",
                        metadata_shard_id
                    )));
                }
            }
            Ok(())
        }
    }
}

fn txn_scope_requires_prepare(scope: &TxnScope) -> bool {
    matches!(scope, TxnScope::MultiShardWrite { .. })
}

fn validate_shard_placements(shard_id: ShardId, placements: &[ShardPlacement]) -> DbResult<()> {
    if placements.is_empty() {
        return Err(aiondb_core::DbError::internal(format!(
            "shard {shard_id} has no replica placements"
        )));
    }

    let mut node_ids = HashSet::new();
    let mut leader_count = 0usize;
    for placement in placements {
        if placement.shard_id != shard_id {
            return Err(aiondb_core::DbError::internal(format!(
                "placement for shard {} cannot be stored under shard {}",
                placement.shard_id, shard_id
            )));
        }
        if !node_ids.insert(placement.node_id.clone()) {
            return Err(aiondb_core::DbError::internal(format!(
                "shard {shard_id} has duplicate placement for node {}",
                placement.node_id
            )));
        }
        if placement.role == ReplicaRole::Leader {
            leader_count += 1;
        }
    }

    match leader_count {
        1 => Ok(()),
        0 => Err(aiondb_core::DbError::internal(format!(
            "shard {shard_id} has no leader placement"
        ))),
        _ => Err(aiondb_core::DbError::internal(format!(
            "shard {shard_id} has multiple leader placements"
        ))),
    }
}

fn ensure_node_live_if_registered(
    state: &InMemoryControlPlaneState,
    node_id: &NodeId,
) -> DbResult<()> {
    if let Some(node) = state.nodes.get(node_id) {
        if !node.is_live {
            return Err(aiondb_core::DbError::internal(format!(
                "node {node_id} is not live"
            )));
        }
    }
    Ok(())
}

fn node_live_if_registered(state: &InMemoryControlPlaneState, node_id: &NodeId) -> bool {
    match state.nodes.get(node_id) {
        Some(node) => node.is_live,
        None => true,
    }
}

fn voting_replica_count(shard: &ShardDescriptor) -> usize {
    shard
        .placements
        .iter()
        .filter(|placement| placement.role.is_voting())
        .count()
}

fn live_voting_replica_count(state: &InMemoryControlPlaneState, shard: &ShardDescriptor) -> usize {
    shard
        .placements
        .iter()
        .filter(|placement| {
            placement.role.is_voting() && node_live_if_registered(state, &placement.node_id)
        })
        .count()
}

fn majority_quorum_size(voting_replicas: usize) -> usize {
    voting_replicas / 2 + 1
}

fn ensure_live_voting_quorum(
    state: &InMemoryControlPlaneState,
    shard: &ShardDescriptor,
) -> DbResult<()> {
    let voting = voting_replica_count(shard);
    let live = live_voting_replica_count(state, shard);
    let required = majority_quorum_size(voting);
    if live < required {
        return Err(aiondb_core::DbError::internal(format!(
            "shard {} has no live voting quorum ({live}/{voting}, required {required})",
            shard.shard_id
        )));
    }
    Ok(())
}

impl MetadataReader for InMemoryControlPlane {
    fn catalog_version(&self) -> DbResult<CatalogVersion> {
        Ok(self.lock_read()?.catalog_version)
    }

    fn placement_epoch(&self) -> DbResult<PlacementEpoch> {
        Ok(self.lock_read()?.placement_epoch)
    }

    fn database_shards(&self, database_id: DatabaseId) -> DbResult<Vec<ShardDescriptor>> {
        let guard = self.lock_read()?;
        Ok(guard
            .shards
            .values()
            .filter(|shard| shard.database_id == database_id)
            .cloned()
            .collect())
    }

    fn table_shards(
        &self,
        database_id: DatabaseId,
        table_id: RelationId,
    ) -> DbResult<Vec<ShardDescriptor>> {
        let guard = self.lock_read()?;
        let key = (database_id, table_id);
        let Some(shard_ids) = guard.shards_by_table.get(&key) else {
            return Ok(Vec::new());
        };
        let mut shards = Vec::with_capacity(shard_ids.len());
        for shard_id in shard_ids {
            let shard = guard
                .shards
                .get(&shard_map_key_for(database_id, table_id, *shard_id))
                .ok_or_else(|| {
                    aiondb_core::DbError::internal(format!(
                        "table shard index references missing shard {shard_id}"
                    ))
                })?;
            shards.push(shard.clone());
        }
        Ok(shards)
    }
}

impl MetadataWriter for InMemoryControlPlane {
    fn upsert_shard(&self, shard: ShardDescriptor) -> DbResult<()> {
        validate_shard_descriptor(&shard)?;
        let mut guard = self.lock_write()?;
        let key = (shard.database_id, shard.table_id);
        let shard_id = shard.shard_id;
        guard.shards.insert(shard_map_key(&shard), shard);
        let shard_ids = guard.shards_by_table.entry(key).or_default();
        if !shard_ids.contains(&shard_id) {
            shard_ids.push(shard_id);
            shard_ids.sort_unstable();
        }
        guard.catalog_version = next_catalog_version(guard.catalog_version);
        Ok(())
    }

    fn remove_table_shards(&self, database_id: DatabaseId, table_id: RelationId) -> DbResult<()> {
        let mut guard = self.lock_write()?;
        let key = (database_id, table_id);
        let Some(shard_ids) = guard.shards_by_table.remove(&key) else {
            return Ok(());
        };
        let mut removed_any = false;
        for shard_id in shard_ids {
            if guard
                .shards
                .remove(&shard_map_key_for(database_id, table_id, shard_id))
                .is_some()
            {
                removed_any = true;
            }
        }
        if removed_any {
            guard.catalog_version = next_catalog_version(guard.catalog_version);
            guard.placement_epoch = next_placement_epoch(guard.placement_epoch);
        }
        Ok(())
    }

    fn update_placement(
        &self,
        shard_id: ShardId,
        placements: Vec<ShardPlacement>,
    ) -> DbResult<PlacementEpoch> {
        let mut guard = self.lock_write()?;
        let shard_key = unique_shard_map_key_for_id(&guard, shard_id)?
            .ok_or_else(|| aiondb_core::DbError::internal(format!("unknown shard {shard_id}")))?;
        validate_shard_placements(shard_id, &placements)?;
        let next_epoch = next_placement_epoch(guard.placement_epoch);
        let shard = guard
            .shards
            .get_mut(&shard_key)
            .ok_or_else(|| aiondb_core::DbError::internal(format!("unknown shard {shard_id}")))?;
        shard.placements = placements
            .into_iter()
            .map(|mut placement| {
                placement.lease_epoch = next_epoch;
                placement
            })
            .collect();
        guard.placement_epoch = next_epoch;
        Ok(next_epoch)
    }

    fn update_table_shard_placement(
        &self,
        database_id: DatabaseId,
        table_id: RelationId,
        shard_id: ShardId,
        placements: Vec<ShardPlacement>,
    ) -> DbResult<PlacementEpoch> {
        let mut guard = self.lock_write()?;
        let shard_key = shard_map_key_for(database_id, table_id, shard_id);
        if !guard.shards.contains_key(&shard_key) {
            return Err(aiondb_core::DbError::internal(format!(
                "unknown shard {shard_id} for table {table_id:?}"
            )));
        }
        validate_shard_placements(shard_id, &placements)?;
        let next_epoch = next_placement_epoch(guard.placement_epoch);
        let shard = guard
            .shards
            .get_mut(&shard_key)
            .ok_or_else(|| aiondb_core::DbError::internal(format!("unknown shard {shard_id}")))?;
        shard.placements = placements
            .into_iter()
            .map(|mut placement| {
                placement.lease_epoch = next_epoch;
                placement
            })
            .collect();
        guard.placement_epoch = next_epoch;
        Ok(next_epoch)
    }
}

impl NodeMembership for InMemoryControlPlane {
    fn upsert_node(&self, node: NodeDescriptor) -> DbResult<PlacementEpoch> {
        validate_node_descriptor(&node)?;
        let mut guard = self.lock_write()?;
        if guard.nodes.get(&node.node_id) == Some(&node) {
            return Ok(guard.placement_epoch);
        }

        let next_epoch = next_placement_epoch(guard.placement_epoch);
        guard.nodes.insert(node.node_id.clone(), node);
        guard.placement_epoch = next_epoch;
        Ok(next_epoch)
    }

    fn mark_node_live(&self, node_id: &NodeId, is_live: bool) -> DbResult<PlacementEpoch> {
        let mut guard = self.lock_write()?;
        let current_epoch = guard.placement_epoch;
        let node = guard
            .nodes
            .get_mut(node_id)
            .ok_or_else(|| aiondb_core::DbError::internal(format!("unknown node {node_id}")))?;
        if node.is_live == is_live {
            return Ok(current_epoch);
        }

        let next_epoch = next_placement_epoch(current_epoch);
        node.is_live = is_live;
        guard.placement_epoch = next_epoch;
        Ok(next_epoch)
    }

    fn node(&self, node_id: &NodeId) -> DbResult<Option<NodeDescriptor>> {
        Ok(self.lock_read()?.nodes.get(node_id).cloned())
    }

    fn nodes(&self) -> DbResult<Vec<NodeDescriptor>> {
        Ok(self.lock_read()?.nodes.values().cloned().collect())
    }

    fn live_nodes(&self) -> DbResult<Vec<NodeDescriptor>> {
        Ok(self
            .lock_read()?
            .nodes
            .values()
            .filter(|node| node.is_live)
            .cloned()
            .collect())
    }
}

impl ShardResolver for InMemoryControlPlane {
    fn resolve_read_shards(
        &self,
        database_id: DatabaseId,
        table_id: RelationId,
    ) -> DbResult<Vec<ShardPlacement>> {
        let guard = self.lock_read()?;
        let key = (database_id, table_id);
        let Some(shard_ids) = guard.shards_by_table.get(&key) else {
            return Ok(Vec::new());
        };
        let mut placements = Vec::with_capacity(shard_ids.len());
        for shard_id in shard_ids {
            let shard = guard
                .shards
                .get(&shard_map_key_for(database_id, table_id, *shard_id))
                .ok_or_else(|| {
                    aiondb_core::DbError::internal(format!(
                        "table shard index references missing shard {shard_id}"
                    ))
                })?;
            let placement = leader_placement_required(shard)?;
            ensure_node_live_if_registered(&guard, &placement.node_id)?;
            ensure_live_voting_quorum(&guard, shard)?;
            placements.push(placement);
        }
        Ok(placements)
    }

    fn resolve_write_shard(
        &self,
        database_id: DatabaseId,
        table_id: RelationId,
        shard_key: &[u8],
    ) -> DbResult<ShardPlacement> {
        let guard = self.lock_read()?;
        let key = (database_id, table_id);
        let Some(shard_ids) = guard.shards_by_table.get(&key) else {
            return Err(aiondb_core::DbError::internal(format!(
                "no shards registered for table {table_id:?}"
            )));
        };
        if shard_ids.is_empty() {
            return Err(aiondb_core::DbError::internal(format!(
                "no shards registered for table {table_id:?}"
            )));
        }
        let hash = shard_key_hash(shard_key);
        let index = usize::try_from(hash).unwrap_or_else(|_| {
            let folded = (hash ^ (hash >> 32)) & u64::from(u32::MAX);
            usize::try_from(folded).unwrap_or(usize::MAX)
        }) % shard_ids.len();
        let shard_id = shard_ids[index];
        let shard = guard
            .shards
            .get(&shard_map_key_for(database_id, table_id, shard_id))
            .ok_or_else(|| {
                aiondb_core::DbError::internal(format!(
                    "table shard index references missing shard {shard_id}"
                ))
            })?;
        let placement = leader_placement_required(shard)?;
        ensure_node_live_if_registered(&guard, &placement.node_id)?;
        ensure_live_voting_quorum(&guard, shard)?;
        Ok(placement)
    }
}

impl ReplicaController for InMemoryControlPlane {
    fn leader_for(&self, shard_id: ShardId) -> DbResult<Option<NodeId>> {
        let guard = self.lock_read()?;
        let Some(shard_key) = unique_shard_map_key_for_id(&guard, shard_id)? else {
            return Ok(None);
        };
        let shard = guard
            .shards
            .get(&shard_key)
            .ok_or_else(|| aiondb_core::DbError::internal(format!("unknown shard {shard_id}")))?;
        let placement = leader_placement_required(shard)?;
        ensure_node_live_if_registered(&guard, &placement.node_id)?;
        ensure_live_voting_quorum(&guard, shard)?;
        Ok(Some(placement.node_id))
    }

    fn transfer_leadership(&self, shard_id: ShardId, target: NodeId) -> DbResult<PlacementEpoch> {
        let mut guard = self.lock_write()?;
        let shard_key = unique_shard_map_key_for_id(&guard, shard_id)?
            .ok_or_else(|| aiondb_core::DbError::internal(format!("unknown shard {shard_id}")))?;
        {
            let shard = guard.shards.get(&shard_key).ok_or_else(|| {
                aiondb_core::DbError::internal(format!("unknown shard {shard_id}"))
            })?;
            leader_placement_required(shard)?;
            ensure_live_voting_quorum(&guard, shard)?;
            let target_placement = shard
                .placements
                .iter()
                .find(|placement| placement.node_id == target)
                .ok_or_else(|| {
                    aiondb_core::DbError::internal(format!(
                        "target node {target} is not a replica for shard {shard_id}"
                    ))
                })?;
            if !target_placement.role.is_voting() {
                return Err(aiondb_core::DbError::internal(format!(
                    "target node {target} is not a voting replica for shard {shard_id}"
                )));
            }
            ensure_node_live_if_registered(&guard, &target)?;
        }
        let next_epoch = next_placement_epoch(guard.placement_epoch);
        let shard = guard
            .shards
            .get_mut(&shard_key)
            .ok_or_else(|| aiondb_core::DbError::internal(format!("unknown shard {shard_id}")))?;
        for placement in &mut shard.placements {
            placement.role = if placement.node_id == target {
                ReplicaRole::Leader
            } else if placement.role == ReplicaRole::Leader {
                ReplicaRole::Follower
            } else {
                placement.role
            };
            placement.lease_epoch = next_epoch;
        }
        guard.placement_epoch = next_epoch;
        Ok(next_epoch)
    }

    fn current_lease(&self, shard_id: ShardId) -> DbResult<Option<EpochLease>> {
        let guard = self.lock_read()?;
        let Some(shard_key) = unique_shard_map_key_for_id(&guard, shard_id)? else {
            return Ok(None);
        };
        let shard = guard
            .shards
            .get(&shard_key)
            .ok_or_else(|| aiondb_core::DbError::internal(format!("unknown shard {shard_id}")))?;
        let placement = leader_placement_required(shard)?;
        ensure_node_live_if_registered(&guard, &placement.node_id)?;
        ensure_live_voting_quorum(&guard, shard)?;
        Ok(Some(EpochLease {
            shard_id,
            leader: placement.node_id,
            epoch: placement.lease_epoch,
            expires_after: None,
        }))
    }
}

impl TxnCoordinator for InMemoryTxnCoordinator {
    fn begin(&self, scope: TxnScope) -> DbResult<TxnId> {
        validate_txn_scope(&scope)?;
        let mut guard = self.lock_write()?;
        let txn_id = TxnId::new(guard.next_txn_id);
        guard.next_txn_id = guard
            .next_txn_id
            .checked_add(1)
            .ok_or_else(|| aiondb_core::DbError::internal("transaction id counter overflow"))?;
        guard.records.insert(
            txn_id,
            TxnRecord {
                txn_id,
                scope,
                status: TxnRecordStatus::Active,
                participants: Vec::new(),
                decision: None,
            },
        );
        Ok(txn_id)
    }

    fn allocate_read_snapshot(&self, mut shard_ids: Vec<ShardId>) -> DbResult<TxnScope> {
        validate_unique_shards("read-only transaction", &shard_ids)?;
        shard_ids.sort_unstable();
        let mut guard = self.lock_write()?;
        let snapshot_ts = SnapshotTimestamp::new(guard.next_snapshot_ts);
        guard.next_snapshot_ts = guard
            .next_snapshot_ts
            .checked_add(1)
            .ok_or_else(|| aiondb_core::DbError::internal("snapshot timestamp overflow"))?;
        Ok(TxnScope::ReadOnlyMultiShard {
            snapshot_ts,
            shard_ids,
        })
    }

    fn prepare(&self, txn_id: TxnId, participants: &[TxnParticipant]) -> DbResult<()> {
        let mut guard = self.lock_write()?;
        let record = guard
            .records
            .get_mut(&txn_id)
            .ok_or_else(|| aiondb_core::DbError::internal(format!("unknown txn {txn_id:?}")))?;
        match record.status {
            TxnRecordStatus::Active => {
                validate_txn_participants(&record.scope, participants)?;
                record.participants = participants.to_vec();
                record.status = TxnRecordStatus::Prepared;
                Ok(())
            }
            TxnRecordStatus::Prepared if record.participants == participants => Ok(()),
            TxnRecordStatus::Prepared => Err(aiondb_core::DbError::internal(format!(
                "txn {txn_id:?} was already prepared with different participants"
            ))),
            TxnRecordStatus::Committed | TxnRecordStatus::Aborted => Err(
                aiondb_core::DbError::internal(format!("txn {txn_id:?} is already decided")),
            ),
        }
    }

    fn decide(&self, txn_id: TxnId, decision: TxnDecision) -> DbResult<()> {
        let mut guard = self.lock_write()?;
        let record = guard
            .records
            .get_mut(&txn_id)
            .ok_or_else(|| aiondb_core::DbError::internal(format!("unknown txn {txn_id:?}")))?;

        match (record.status, record.decision) {
            (TxnRecordStatus::Committed | TxnRecordStatus::Aborted, Some(existing))
                if existing == decision =>
            {
                return Ok(());
            }
            (TxnRecordStatus::Committed | TxnRecordStatus::Aborted, Some(_)) => {
                return Err(aiondb_core::DbError::internal(format!(
                    "txn {txn_id:?} is already decided"
                )));
            }
            _ => {}
        }

        if matches!(decision, TxnDecision::Commit { .. })
            && txn_scope_requires_prepare(&record.scope)
            && record.status != TxnRecordStatus::Prepared
        {
            return Err(aiondb_core::DbError::internal(format!(
                "txn {txn_id:?} must be prepared before commit"
            )));
        }

        record.status = match decision {
            TxnDecision::Commit { .. } => TxnRecordStatus::Committed,
            TxnDecision::Abort => TxnRecordStatus::Aborted,
        };
        record.decision = Some(decision);
        Ok(())
    }

    fn abort(&self, txn_id: TxnId) -> DbResult<()> {
        self.decide(txn_id, TxnDecision::Abort)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement(shard_id: u32, node_id: &str, role: ReplicaRole) -> ShardPlacement {
        ShardPlacement {
            shard_id: ShardId::new(shard_id),
            node_id: NodeId::new(node_id),
            role,
            lease_epoch: PlacementEpoch::default(),
        }
    }

    fn shard(shard_id: u32, table_id: RelationId, node_id: &str) -> ShardDescriptor {
        ShardDescriptor {
            database_id: DatabaseId::DEFAULT,
            table_id,
            shard_id: ShardId::new(shard_id),
            placements: vec![placement(shard_id, node_id, ReplicaRole::Leader)],
        }
    }

    fn participant(shard_id: u32, leader: &str) -> TxnParticipant {
        TxnParticipant {
            shard_id: ShardId::new(shard_id),
            leader: NodeId::new(leader),
        }
    }

    fn node(node_id: &str, endpoint: &str, is_live: bool) -> NodeDescriptor {
        NodeDescriptor {
            node_id: NodeId::new(node_id),
            rpc_endpoint: endpoint.to_owned(),
            is_live,
        }
    }

    #[test]
    fn upsert_node_tracks_membership_and_liveness() {
        let plane = InMemoryControlPlane::new();

        let epoch = plane
            .upsert_node(node("node-a", "127.0.0.1:9001", true))
            .unwrap();

        assert_eq!(epoch, PlacementEpoch::new(1));
        assert_eq!(plane.placement_epoch().unwrap(), PlacementEpoch::new(1));
        assert_eq!(
            plane
                .node(&NodeId::new("node-a"))
                .unwrap()
                .unwrap()
                .rpc_endpoint,
            "127.0.0.1:9001"
        );
        assert_eq!(plane.live_nodes().unwrap().len(), 1);
    }

    #[test]
    fn snapshot_reports_nodes_shards_and_epochs_in_node_order() {
        let plane = InMemoryControlPlane::new();
        plane
            .upsert_node(node("node-b", "127.0.0.1:9002", false))
            .unwrap();
        plane
            .upsert_node(node("node-a", "127.0.0.1:9001", true))
            .unwrap();
        plane
            .upsert_shard(shard(2, RelationId::new(42), "node-a"))
            .unwrap();

        let snapshot = plane.snapshot().unwrap();

        assert_eq!(snapshot.catalog_version, CatalogVersion::new(1));
        assert_eq!(snapshot.placement_epoch, PlacementEpoch::new(2));
        assert_eq!(snapshot.total_nodes, 2);
        assert_eq!(snapshot.live_nodes, 1);
        assert_eq!(snapshot.total_shards, 1);
        assert_eq!(snapshot.nodes[0].node_id, NodeId::new("node-a"));
        assert_eq!(snapshot.nodes[1].node_id, NodeId::new("node-b"));
    }

    #[test]
    fn upsert_node_is_idempotent_for_identical_descriptor() {
        let plane = InMemoryControlPlane::new();
        let descriptor = node("node-a", "127.0.0.1:9001", true);

        assert_eq!(
            plane.upsert_node(descriptor.clone()).unwrap(),
            PlacementEpoch::new(1)
        );
        assert_eq!(
            plane.upsert_node(descriptor).unwrap(),
            PlacementEpoch::new(1)
        );
        assert_eq!(plane.placement_epoch().unwrap(), PlacementEpoch::new(1));
    }

    #[test]
    fn mark_node_live_updates_epoch_only_on_change() {
        let plane = InMemoryControlPlane::new();
        plane
            .upsert_node(node("node-a", "127.0.0.1:9001", true))
            .unwrap();

        assert_eq!(
            plane.mark_node_live(&NodeId::new("node-a"), true).unwrap(),
            PlacementEpoch::new(1)
        );
        assert_eq!(
            plane.mark_node_live(&NodeId::new("node-a"), false).unwrap(),
            PlacementEpoch::new(2)
        );

        assert!(plane.live_nodes().unwrap().is_empty());
        assert!(!plane.node(&NodeId::new("node-a")).unwrap().unwrap().is_live);
    }

    #[test]
    fn mark_node_live_rejects_unknown_node_without_epoch_bump() {
        let plane = InMemoryControlPlane::new();

        let error = plane
            .mark_node_live(&NodeId::new("missing"), false)
            .expect_err("unknown node must fail");

        assert!(error.to_string().contains("unknown node missing"));
        assert_eq!(plane.placement_epoch().unwrap(), PlacementEpoch::default());
    }

    #[test]
    fn upsert_node_rejects_empty_endpoint() {
        let plane = InMemoryControlPlane::new();

        let error = plane
            .upsert_node(node("node-a", "", true))
            .expect_err("empty endpoint must fail");

        assert!(error.to_string().contains("empty rpc endpoint"));
        assert_eq!(plane.placement_epoch().unwrap(), PlacementEpoch::default());
    }

    #[test]
    fn upsert_shard_indexes_by_database_and_table() {
        let plane = InMemoryControlPlane::new();
        plane
            .upsert_shard(shard(1, RelationId::new(42), "node-a"))
            .unwrap();

        let shards = plane
            .table_shards(DatabaseId::DEFAULT, RelationId::new(42))
            .unwrap();
        assert_eq!(shards.len(), 1);
        assert_eq!(shards[0].shard_id, ShardId::new(1));
        assert_eq!(plane.catalog_version().unwrap(), CatalogVersion::new(1));
    }

    #[test]
    fn upsert_shard_keeps_same_logical_shard_id_on_different_tables() {
        let plane = InMemoryControlPlane::new();
        plane
            .upsert_shard(shard(1, RelationId::new(42), "node-a"))
            .unwrap();
        plane
            .upsert_shard(shard(1, RelationId::new(43), "node-a"))
            .unwrap();

        assert_eq!(
            plane
                .table_shards(DatabaseId::DEFAULT, RelationId::new(42))
                .unwrap()
                .len(),
            1
        );
        let moved = plane
            .table_shards(DatabaseId::DEFAULT, RelationId::new(43))
            .unwrap();
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].shard_id, ShardId::new(1));
        assert_eq!(plane.catalog_version().unwrap(), CatalogVersion::new(2));
    }

    #[test]
    fn remove_table_shards_removes_primary_and_secondary_indexes() {
        let plane = InMemoryControlPlane::new();
        plane
            .upsert_shard(shard(1, RelationId::new(42), "node-a"))
            .unwrap();
        plane
            .upsert_shard(shard(2, RelationId::new(42), "node-a"))
            .unwrap();
        assert_eq!(plane.snapshot().unwrap().total_shards, 2);

        plane
            .remove_table_shards(DatabaseId::DEFAULT, RelationId::new(42))
            .unwrap();

        assert!(plane
            .table_shards(DatabaseId::DEFAULT, RelationId::new(42))
            .unwrap()
            .is_empty());
        assert_eq!(plane.snapshot().unwrap().total_shards, 0);
        assert_eq!(plane.catalog_version().unwrap(), CatalogVersion::new(3));
        assert_eq!(plane.placement_epoch().unwrap(), PlacementEpoch::new(1));
    }

    #[test]
    fn table_shards_rejects_stale_secondary_index() {
        let plane = InMemoryControlPlane::new();
        {
            let mut guard = plane.lock_write().unwrap();
            guard.shards_by_table.insert(
                (DatabaseId::DEFAULT, RelationId::new(42)),
                vec![ShardId::new(9)],
            );
        }

        let error = plane
            .table_shards(DatabaseId::DEFAULT, RelationId::new(42))
            .expect_err("stale shard index must fail");

        assert!(error
            .to_string()
            .contains("table shard index references missing shard shard-9"));
    }

    #[test]
    fn upsert_shard_rejects_missing_leader() {
        let plane = InMemoryControlPlane::new();
        let descriptor = ShardDescriptor {
            database_id: DatabaseId::DEFAULT,
            table_id: RelationId::new(42),
            shard_id: ShardId::new(1),
            placements: vec![placement(1, "node-a", ReplicaRole::Follower)],
        };

        let error = plane
            .upsert_shard(descriptor)
            .expect_err("shard without leader must fail");

        assert!(error.to_string().contains("has no leader placement"));
    }

    #[test]
    fn upsert_shard_rejects_multiple_leaders() {
        let plane = InMemoryControlPlane::new();
        let mut descriptor = shard(1, RelationId::new(42), "node-a");
        descriptor
            .placements
            .push(placement(1, "node-b", ReplicaRole::Leader));

        let error = plane
            .upsert_shard(descriptor)
            .expect_err("shard with multiple leaders must fail");

        assert!(error.to_string().contains("multiple leader placements"));
    }

    #[test]
    fn upsert_shard_rejects_duplicate_node_placements() {
        let plane = InMemoryControlPlane::new();
        let mut descriptor = shard(1, RelationId::new(42), "node-a");
        descriptor
            .placements
            .push(placement(1, "node-a", ReplicaRole::Follower));

        let error = plane
            .upsert_shard(descriptor)
            .expect_err("duplicate node placement must fail");

        assert!(error
            .to_string()
            .contains("duplicate placement for node node-a"));
    }

    #[test]
    fn upsert_shard_rejects_mismatched_placement_shard_id() {
        let plane = InMemoryControlPlane::new();
        let descriptor = ShardDescriptor {
            database_id: DatabaseId::DEFAULT,
            table_id: RelationId::new(42),
            shard_id: ShardId::new(1),
            placements: vec![placement(2, "node-a", ReplicaRole::Leader)],
        };

        let error = plane
            .upsert_shard(descriptor)
            .expect_err("mismatched placement shard id must fail");

        assert!(error
            .to_string()
            .contains("placement for shard shard-2 cannot be stored under shard shard-1"));
    }

    #[test]
    fn update_placement_rejects_invalid_leader_set() {
        let plane = InMemoryControlPlane::new();
        plane
            .upsert_shard(shard(1, RelationId::new(42), "node-a"))
            .unwrap();

        let error = plane
            .update_placement(
                ShardId::new(1),
                vec![placement(1, "node-a", ReplicaRole::Follower)],
            )
            .expect_err("placement update without leader must fail");

        assert!(error.to_string().contains("has no leader placement"));
        assert_eq!(plane.placement_epoch().unwrap(), PlacementEpoch::default());
    }

    #[test]
    fn update_placement_reports_unknown_shard_before_placement_validation() {
        let plane = InMemoryControlPlane::new();

        let error = plane
            .update_placement(ShardId::new(7), Vec::new())
            .expect_err("unknown shard must fail before validating placements");

        assert!(error.to_string().contains("unknown shard shard-7"));
    }

    #[test]
    fn resolve_read_shards_returns_leaders() {
        let plane = InMemoryControlPlane::new();
        plane
            .upsert_shard(shard(1, RelationId::new(42), "node-a"))
            .unwrap();
        plane
            .upsert_shard(shard(2, RelationId::new(42), "node-b"))
            .unwrap();

        let placements = plane
            .resolve_read_shards(DatabaseId::DEFAULT, RelationId::new(42))
            .unwrap();
        assert_eq!(placements.len(), 2);
        assert!(placements.iter().all(|p| p.role == ReplicaRole::Leader));
    }

    #[test]
    fn resolve_read_shards_rejects_registered_down_leader() {
        let plane = InMemoryControlPlane::new();
        plane
            .upsert_node(node("node-a", "127.0.0.1:9001", false))
            .unwrap();
        plane
            .upsert_shard(shard(1, RelationId::new(42), "node-a"))
            .unwrap();

        let error = plane
            .resolve_read_shards(DatabaseId::DEFAULT, RelationId::new(42))
            .expect_err("down registered leader must fail reads");

        assert!(error.to_string().contains("node node-a is not live"));
    }

    #[test]
    fn resolve_write_shard_rejects_registered_down_leader() {
        let plane = InMemoryControlPlane::new();
        plane
            .upsert_node(node("node-a", "127.0.0.1:9001", false))
            .unwrap();
        plane
            .upsert_shard(shard(1, RelationId::new(42), "node-a"))
            .unwrap();

        let error = plane
            .resolve_write_shard(DatabaseId::DEFAULT, RelationId::new(42), b"key")
            .expect_err("down registered leader must fail writes");

        assert!(error.to_string().contains("node node-a is not live"));
    }

    #[test]
    fn resolve_write_shard_rejects_lost_voting_quorum() {
        let plane = InMemoryControlPlane::new();
        plane
            .upsert_node(node("node-a", "127.0.0.1:9001", true))
            .unwrap();
        plane
            .upsert_node(node("node-b", "127.0.0.1:9002", false))
            .unwrap();
        plane
            .upsert_node(node("node-c", "127.0.0.1:9003", false))
            .unwrap();
        let mut descriptor = shard(1, RelationId::new(42), "node-a");
        descriptor
            .placements
            .push(placement(1, "node-b", ReplicaRole::Follower));
        descriptor
            .placements
            .push(placement(1, "node-c", ReplicaRole::Follower));
        plane.upsert_shard(descriptor).unwrap();

        let error = plane
            .resolve_write_shard(DatabaseId::DEFAULT, RelationId::new(42), b"tenant-1")
            .expect_err("lost majority must reject writes");

        assert!(error.to_string().contains("no live voting quorum"));
    }

    #[test]
    fn leader_and_lease_reject_registered_down_leader() {
        let plane = InMemoryControlPlane::new();
        plane
            .upsert_node(node("node-a", "127.0.0.1:9001", false))
            .unwrap();
        plane
            .upsert_shard(shard(1, RelationId::new(42), "node-a"))
            .unwrap();

        let leader_error = plane
            .leader_for(ShardId::new(1))
            .expect_err("down registered leader must fail leader lookup");
        let lease_error = plane
            .current_lease(ShardId::new(1))
            .expect_err("down registered leader must fail lease lookup");

        assert!(leader_error.to_string().contains("node node-a is not live"));
        assert!(lease_error.to_string().contains("node node-a is not live"));
    }

    #[test]
    fn leader_lookup_rejects_existing_shard_without_leader() {
        let plane = InMemoryControlPlane::new();
        {
            let mut guard = plane.lock_write().unwrap();
            guard.shards.insert(
                shard_map_key_for(DatabaseId::DEFAULT, RelationId::new(42), ShardId::new(1)),
                ShardDescriptor {
                    database_id: DatabaseId::DEFAULT,
                    table_id: RelationId::new(42),
                    shard_id: ShardId::new(1),
                    placements: vec![placement(1, "node-a", ReplicaRole::Follower)],
                },
            );
        }

        let error = plane
            .leader_for(ShardId::new(1))
            .expect_err("existing shard without leader must fail");

        assert!(error.to_string().contains("has no leader placement"));
    }

    #[test]
    fn transfer_leadership_updates_epoch_and_role() {
        let plane = InMemoryControlPlane::new();
        let mut descriptor = shard(1, RelationId::new(42), "node-a");
        descriptor
            .placements
            .push(placement(1, "node-b", ReplicaRole::Follower));
        plane.upsert_shard(descriptor).unwrap();

        let epoch = plane
            .transfer_leadership(ShardId::new(1), NodeId::new("node-b"))
            .unwrap();

        assert_eq!(epoch, PlacementEpoch::new(1));
        assert_eq!(
            plane.leader_for(ShardId::new(1)).unwrap(),
            Some(NodeId::new("node-b"))
        );
        assert_eq!(
            plane.current_lease(ShardId::new(1)).unwrap().unwrap().epoch,
            epoch
        );
    }

    #[test]
    fn transfer_leadership_recovers_from_down_leader_when_quorum_survives() {
        let plane = InMemoryControlPlane::new();
        plane
            .upsert_node(node("node-a", "127.0.0.1:9001", false))
            .unwrap();
        plane
            .upsert_node(node("node-b", "127.0.0.1:9002", true))
            .unwrap();
        plane
            .upsert_node(node("node-c", "127.0.0.1:9003", true))
            .unwrap();
        let mut descriptor = shard(1, RelationId::new(42), "node-a");
        descriptor
            .placements
            .push(placement(1, "node-b", ReplicaRole::Follower));
        descriptor
            .placements
            .push(placement(1, "node-c", ReplicaRole::Follower));
        plane.upsert_shard(descriptor).unwrap();

        let epoch = plane
            .transfer_leadership(ShardId::new(1), NodeId::new("node-b"))
            .unwrap();

        assert_eq!(epoch, PlacementEpoch::new(4));
        assert_eq!(
            plane.leader_for(ShardId::new(1)).unwrap(),
            Some(NodeId::new("node-b"))
        );
    }

    #[test]
    fn transfer_leadership_rejects_learner_target_and_preserves_role() {
        let plane = InMemoryControlPlane::new();
        plane
            .upsert_node(node("node-a", "127.0.0.1:9001", true))
            .unwrap();
        plane
            .upsert_node(node("node-b", "127.0.0.1:9002", true))
            .unwrap();
        plane
            .upsert_node(node("node-c", "127.0.0.1:9003", true))
            .unwrap();
        let mut descriptor = shard(1, RelationId::new(42), "node-a");
        descriptor
            .placements
            .push(placement(1, "node-b", ReplicaRole::Follower));
        descriptor
            .placements
            .push(placement(1, "node-c", ReplicaRole::Learner));
        plane.upsert_shard(descriptor).unwrap();

        let error = plane
            .transfer_leadership(ShardId::new(1), NodeId::new("node-c"))
            .expect_err("learner cannot become leader through lease transfer");

        assert!(error.to_string().contains("not a voting replica"));
        let shard = plane
            .table_shards(DatabaseId::DEFAULT, RelationId::new(42))
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(
            leader_placement(&shard).unwrap().node_id,
            NodeId::new("node-a")
        );
        assert_eq!(
            shard
                .placements
                .iter()
                .find(|placement| placement.node_id == NodeId::new("node-c"))
                .unwrap()
                .role,
            ReplicaRole::Learner
        );
    }

    #[test]
    fn transfer_leadership_missing_target_does_not_mutate_placements() {
        let plane = InMemoryControlPlane::new();
        let mut descriptor = shard(1, RelationId::new(42), "node-a");
        descriptor
            .placements
            .push(placement(1, "node-b", ReplicaRole::Follower));
        plane.upsert_shard(descriptor).unwrap();

        let error = plane
            .transfer_leadership(ShardId::new(1), NodeId::new("node-c"))
            .expect_err("unknown target must fail");

        assert!(error
            .to_string()
            .contains("target node node-c is not a replica"));
        assert_eq!(plane.placement_epoch().unwrap(), PlacementEpoch::default());
        assert_eq!(
            plane.leader_for(ShardId::new(1)).unwrap(),
            Some(NodeId::new("node-a"))
        );
        let shard = plane
            .table_shards(DatabaseId::DEFAULT, RelationId::new(42))
            .unwrap()
            .remove(0);
        assert_eq!(shard.placements[0].lease_epoch, PlacementEpoch::default());
        assert_eq!(shard.placements[1].lease_epoch, PlacementEpoch::default());
    }

    #[test]
    fn txn_coordinator_records_begin_prepare_and_commit() {
        let coordinator = InMemoryTxnCoordinator::new();
        let txn_id = coordinator
            .begin(TxnScope::MultiShardWrite {
                coordinator: NodeId::new("node-a"),
                participant_shards: vec![ShardId::new(1), ShardId::new(2)],
            })
            .unwrap();
        let participants = vec![participant(1, "node-a"), participant(2, "node-b")];

        coordinator.prepare(txn_id, &participants).unwrap();
        coordinator
            .decide(
                txn_id,
                TxnDecision::Commit {
                    commit_ts: SnapshotTimestamp::new(99),
                },
            )
            .unwrap();

        let record = coordinator.record(txn_id).unwrap().unwrap();
        assert_eq!(record.status, TxnRecordStatus::Committed);
        assert_eq!(record.participants, participants);
        assert_eq!(
            record.decision,
            Some(TxnDecision::Commit {
                commit_ts: SnapshotTimestamp::new(99)
            })
        );
    }

    #[test]
    fn txn_coordinator_rejects_multi_shard_commit_before_prepare() {
        let coordinator = InMemoryTxnCoordinator::new();
        let txn_id = coordinator
            .begin(TxnScope::MultiShardWrite {
                coordinator: NodeId::new("node-a"),
                participant_shards: vec![ShardId::new(1), ShardId::new(2)],
            })
            .unwrap();

        let error = coordinator
            .decide(
                txn_id,
                TxnDecision::Commit {
                    commit_ts: SnapshotTimestamp::new(99),
                },
            )
            .expect_err("multi-shard commit must require prepare");

        assert!(error.to_string().contains("must be prepared before commit"));
        assert_eq!(
            coordinator.record(txn_id).unwrap().unwrap().status,
            TxnRecordStatus::Active
        );
    }

    #[test]
    fn txn_coordinator_prepare_is_idempotent_for_same_participants() {
        let coordinator = InMemoryTxnCoordinator::new();
        let txn_id = coordinator
            .begin(TxnScope::SingleShard {
                shard_id: ShardId::new(7),
            })
            .unwrap();
        let participants = vec![participant(7, "node-a")];

        coordinator.prepare(txn_id, &participants).unwrap();
        coordinator.prepare(txn_id, &participants).unwrap();

        assert_eq!(
            coordinator.record(txn_id).unwrap().unwrap().status,
            TxnRecordStatus::Prepared
        );
    }

    #[test]
    fn txn_coordinator_rejects_participants_outside_scope() {
        let coordinator = InMemoryTxnCoordinator::new();
        let txn_id = coordinator
            .begin(TxnScope::SingleShard {
                shard_id: ShardId::new(7),
            })
            .unwrap();

        let error = coordinator
            .prepare(txn_id, &[participant(8, "node-a")])
            .expect_err("wrong participant shard must fail");

        assert!(error.to_string().contains("cannot prepare participants"));
    }

    #[test]
    fn txn_coordinator_abort_is_idempotent() {
        let coordinator = InMemoryTxnCoordinator::new();
        let txn_id = coordinator.begin(TxnScope::Local).unwrap();

        coordinator.abort(txn_id).unwrap();
        coordinator.abort(txn_id).unwrap();

        let record = coordinator.record(txn_id).unwrap().unwrap();
        assert_eq!(record.status, TxnRecordStatus::Aborted);
        assert_eq!(record.decision, Some(TxnDecision::Abort));
    }

    #[test]
    fn txn_coordinator_allocates_monotonic_read_snapshots() {
        let coordinator = InMemoryTxnCoordinator::new();

        let first = coordinator
            .allocate_read_snapshot(vec![ShardId::new(2), ShardId::new(1)])
            .unwrap();
        let second = coordinator
            .allocate_read_snapshot(vec![ShardId::new(1), ShardId::new(2)])
            .unwrap();

        assert_eq!(
            first,
            TxnScope::ReadOnlyMultiShard {
                snapshot_ts: SnapshotTimestamp::new(1),
                shard_ids: vec![ShardId::new(1), ShardId::new(2)],
            }
        );
        assert_eq!(
            second,
            TxnScope::ReadOnlyMultiShard {
                snapshot_ts: SnapshotTimestamp::new(2),
                shard_ids: vec![ShardId::new(1), ShardId::new(2)],
            }
        );
    }

    #[test]
    fn txn_coordinator_rejects_duplicate_read_snapshot_shards() {
        let coordinator = InMemoryTxnCoordinator::new();

        let error = coordinator
            .allocate_read_snapshot(vec![ShardId::new(1), ShardId::new(1)])
            .expect_err("duplicate shard ids must fail");

        assert!(error
            .to_string()
            .contains("duplicate participant shard shard-1"));
    }

    #[test]
    fn txn_coordinator_rejects_duplicate_scope_shards() {
        let coordinator = InMemoryTxnCoordinator::new();

        let error = coordinator
            .begin(TxnScope::ReadOnlyMultiShard {
                snapshot_ts: SnapshotTimestamp::new(1),
                shard_ids: vec![ShardId::new(1), ShardId::new(1)],
            })
            .expect_err("duplicate shard ids must fail");

        assert!(error
            .to_string()
            .contains("duplicate participant shard shard-1"));
    }

    #[test]
    fn txn_scope_fragment_metadata_validation_accepts_matching_read_scope() {
        validate_txn_scope_fragment_metadata(
            "fragment metadata",
            Some(&TxnScope::ReadOnlyMultiShard {
                snapshot_ts: SnapshotTimestamp::new(7),
                shard_ids: vec![ShardId::new(1), ShardId::new(2)],
            }),
            Some(2),
            Some(7),
        )
        .unwrap();
    }

    #[test]
    fn txn_scope_fragment_metadata_validation_rejects_wrong_shard() {
        let error = validate_txn_scope_fragment_metadata(
            "fragment metadata",
            Some(&TxnScope::SingleShard {
                shard_id: ShardId::new(1),
            }),
            Some(2),
            None,
        )
        .expect_err("wrong shard metadata must fail");

        assert!(error
            .to_string()
            .contains("fragment metadata shard_id 2 does not match single-shard txn scope 1"));
    }

    #[test]
    fn txn_scope_fragment_metadata_validation_rejects_wrong_snapshot() {
        let error = validate_txn_scope_fragment_metadata(
            "fragment metadata",
            Some(&TxnScope::ReadOnlyMultiShard {
                snapshot_ts: SnapshotTimestamp::new(7),
                shard_ids: vec![ShardId::new(1)],
            }),
            Some(1),
            Some(8),
        )
        .expect_err("wrong snapshot metadata must fail");

        assert!(error
            .to_string()
            .contains("fragment metadata snapshot_ts 8 does not match read snapshot 7"));
    }
}
