---
title: aiondb-cluster
order: 60
---

# aiondb-cluster

Multi-catalog and multi-storage cluster contracts (ADR-0014). Defines the cluster-level identity types, descriptors, and traits the rest of the engine uses to scope catalog and storage operations to a database, plus the interface-only distributed control-plane contracts that frontends, planners, shard engines, and the fragment transport share. The distributed traits in `distributed` are interface-only; the only concrete control-plane impl shipped here is `InMemoryControlPlane`.

## cargo

```toml
[dependencies]
aiondb-cluster = { path = "../aiondb-cluster" }
```

## modules

| module | purpose |
|---|---|
| `id` | `DatabaseId`, `TablespaceId`. |
| `descriptor` | `DatabaseDescriptor`, `CreateDatabaseRequest`. |
| `role` | `ClusterRoleDescriptor`. |
| `scope` | `DatabaseCatalog`, `DatabaseStorage`, `DatabaseHandle`, `ClusterCatalog`, `InMemoryClusterCatalog`. |
| `distributed` | Cluster control-plane and txn-coordination traits with in-memory implementations. |
| `replication` | Pure replica-placement planners plus maintenance helpers for repair and leadership balancing. |

## key types

### identifiers and descriptors

| type | role |
|---|---|
| `DatabaseId` | `u32` newtype, with `CLUSTER = 0` and `DEFAULT = 1`. |
| `TablespaceId` | `u32` newtype, with `PG_DEFAULT = 1663` and `PG_GLOBAL = 1664`. |
| `DatabaseDescriptor` | persisted metadata for a database, modeled on `pg_database`. |
| `CreateDatabaseRequest` | input to `ClusterCatalog::create_database`. |
| `ClusterRoleDescriptor` | cluster-wide role (mirrors `pg_authid` / `pg_roles`). |
| `DatabaseHandle` | `(DatabaseDescriptor, Arc<dyn DatabaseCatalog>, Arc<dyn DatabaseStorage>)` tuple held by the engine per active database. |

### scope traits

| trait | role |
|---|---|
| `DatabaseCatalog` | marker for a catalog scoped to one database. |
| `DatabaseStorage` | marker for storage scoped to one database. |
| `ClusterCatalog` | source of truth for which databases exist, plus `ALTER DATABASE` operations. |
| `InMemoryClusterCatalog` | default in-memory `ClusterCatalog` implementation. |

### distributed control-plane

| type | role |
|---|---|
| `NodeId` | string-backed cluster node identity. |
| `ShardId` | `u32` shard identifier. |
| `QueryId` | `u128` query identifier. |
| `FragmentId` | `u64` fragment identifier. |
| `CatalogVersion`, `PlacementEpoch`, `SnapshotTimestamp` | monotonic counters. |
| `ReplicaRole` | `Leader`, `Follower`, or `Learner`; leaders and followers are voting replicas. |
| `NodeDescriptor`, `ControlPlaneNodeSnapshot`, `ControlPlaneSnapshot` | membership snapshots. |
| `ShardDescriptor`, `ShardPlacement` | shard placement metadata. |
| `NodeAttributeConstraint`, `ReplicaPlacementPolicy` | placement filters, lease preferences, and failure-domain spread controls. |
| `EpochLease` | leadership lease at a `PlacementEpoch`. |
| `TxnScope`, `TxnParticipant`, `TxnDecision`, `TxnRecord`, `TxnRecordStatus` | distributed transaction record. |
| `FragmentRuntimeOptions` | per-fragment execution caps. |
| `MetadataReader`, `MetadataWriter` | catalog metadata access. |
| `NodeMembership`, `ShardResolver` | membership and placement lookup. |
| `DataPlaneLocalExecutor`, `RemoteExecutor` | local and remote fragment execution. |
| `TxnCoordinator`, `ReplicaController` | distributed txn and replica control. |
| `ControlPlane` | super-trait composing the above. |
| `InMemoryControlPlane`, `InMemoryTxnCoordinator` | in-memory test/default impls. |
| `validate_txn_scope_fragment_metadata` | shared validation helper. |

### replication maintenance

The control plane now enforces Cockroach-style majority safety for read/write shard resolution, lease lookup, and explicit leadership transfer: the current leader must be live and a majority of voting placements must be live. Leadership transfer only targets live voting replicas and preserves learner roles.

The `replication` module exposes `ReplicationMaintenanceOptions`, `ReplicaRepairOptions`, `LeadershipBalanceOptions`, and `ReplicationStatusSnapshot`. `maintain_replication` first repairs safe replica topology drift, then balances hot leaders across live voters. Engine-level `maintain_distributed_replication_from_config` derives the repair factor and learner throttles from `distributed.sharding`, and `mark_distributed_node_live_and_maintain` runs that maintenance automatically when `distributed.sharding.enabled` and `distributed.sharding.auto_rebalance` are both enabled. The repair planner is load-aware: new replicas are assigned to the least-loaded live candidates so replacement work does not pile onto the first available node.

Leadership balancing is rate-limited from runtime config. `AIONDB_SHARDING_LEADERSHIP_MAX_TRANSFERS_PER_MAINTENANCE` caps transfers planned per maintenance pass, while `AIONDB_SHARDING_LEADERSHIP_MIN_LOAD_DELTA` controls how much hotter a live leader host must be before moving leases for balance. Down-leader failover can still transfer leadership when a live voting quorum remains.

`ReplicaRepairMode::LearnerFirst` stages replacements as learners. `maintain_replication_with_caught_up_learners` applies the Cockroach-style second phase by promoting only learners explicitly reported as caught up, then continuing normal repair and leader balance. `caught_up_learner_keys_for_live_nodes` bridges coarser external catch-up signals into shard-specific `ReplicaCatchupKey`s by keeping only live registered learner placements on the reported nodes. The engine wraps this as `maintain_distributed_replication_from_config_with_caught_up_nodes`, so a replica runtime or HA supervisor can report "node X is caught up" without knowing table and shard ids.

`ReplicaPlacementPolicy` adds Cockroach-style placement controls to the pure planner. `required_attributes` filter voter candidates, `lease_preferences` select preferred initial leaders/leaseholders, and `spread_attributes` avoid co-locating voting replicas on the same failure-domain values when alternatives exist. The same policy is used by `plan_initial_shard_replica_placements_with_policy`, `plan_replica_repairs_with_policy`, `plan_leadership_balance_preferences_with_policy`, and the engine's configured replication maintenance path.

Primary-side WAL progress can also feed that bridge through
`maintain_distributed_replication_from_config_with_primary_progress`.
The pgwire replication handler records the startup `application_name` on
each connected replica state; operators should set it to the same value
as the distributed `NodeId` so caught-up WAL receivers can promote their
matching staged learner placements.
When HA and distributed auto-rebalance are both enabled, the server's HA
tick invokes that primary-progress bridge on the current primary.

The server `/metrics` endpoint exports replication health gauges for live quorum, live voters, down voters, total learners, shards with learners, under-replicated shards, and per-node leader/voter/learner load. Replica processes additionally export runtime counters for streaming sessions, reconnects, received WAL bytes, and standby status updates.

## example

```rust
use aiondb_cluster::{
    CreateDatabaseRequest, DatabaseDescriptor, DatabaseId, InMemoryClusterCatalog,
};

let cluster = InMemoryClusterCatalog::new();
cluster.bootstrap_default("postgres").expect("bootstrap default db");
let _all: Vec<DatabaseDescriptor> = cluster
    .list_databases()
    .expect("in-memory catalog never fails on read");

assert_eq!(DatabaseId::DEFAULT.get(), 1);
assert!(DatabaseId::CLUSTER.is_cluster());

let _req = CreateDatabaseRequest {
    name: "analytics".to_owned(),
    owner: "postgres".to_owned(),
    template: None,
    encoding: Some("UTF8".to_owned()),
    collate: None,
    ctype: None,
    tablespace_id: None,
    connection_limit: None,
    is_template: false,
    allow_connections: true,
};
```
