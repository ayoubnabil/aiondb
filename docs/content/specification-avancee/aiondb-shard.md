---
title: aiondb-shard
order: 62
---

# aiondb-shard

Sharding subsystem. Provides consistent-hash automatic sharding and user-defined custom shard keys, plus a `ShardedStorage` wrapper that routes DML to internal per-shard tables. The model is inspired by Qdrant's sharding layer.

The crate also hosts a much larger surface of distributed-execution primitives -- range descriptors, leaseholder loops, anti-entropy, follower reads, CRDT registers, dist sort/topk/group-by, split/transfer executors, and so on. Those modules back the experimental distributed plan; per the v0.1 product contract they are not part of the public single-node release. The table below covers the user-facing single-node sharding API only. The full module set is enumerated in `crates/aiondb-shard/src/lib.rs`.

## cargo

```toml
[dependencies]
aiondb-shard = { path = "../aiondb-shard" }
```

## modules

| module | purpose |
|---|---|
| `config` | `ShardingConfig` and `DEFAULT_*` constants. |
| `shard` | core types: `ShardId`, `ShardKey`, `ShardMetadata`, `ShardState`, `ShardingStrategy`, `NodeAddress`. |
| `hash_ring` | `HashRing` consistent-hash ring built on SHA-256 truncated to 64 bits. |
| `router` | `ShardRouter`: resolves a `ShardKey` to the owning `ShardId`. |
| `manager` | `ShardManager`, `ShardTransferRequest`: lifecycle and rebalancing. |
| `storage` | `ShardedStorage`: shard-aware wrapper around an inner storage engine. |
| `stream` | `MergedTupleStream`: merges per-shard scan streams. |

## key types

| type | role |
|---|---|
| `ShardId` | `u32` shard identifier. |
| `ShardKey` | routing key, either `Numeric(u64)` or `Named(String)`. |
| `NodeAddress` | address of a node hosting one or more shards. |
| `ShardState` | shard lifecycle state. |
| `ShardingStrategy` | `Auto { shard_count, virtual_nodes_per_shard }` or `Custom { shard_key_column }`. |
| `ShardMetadata` | per-shard descriptor stored by the manager. |
| `HashRing` | consistent hash ring with configurable virtual nodes per shard. |
| `ShardRouter` | resolves keys via the ring or an explicit custom map. |
| `ShardManager` | tracks metadata, allocates new shards, plans rebalances. |
| `ShardTransferRequest` | request to move a shard between nodes. |
| `ShardedStorage` | routes DML to per-shard tables; encodes shard index in the high bits of `TupleId`. |
| `MergedTupleStream` | tuple stream that interleaves multiple per-shard streams. |
| `ShardingConfig` | top-level on/off plus default shard count and replication factor. |

`ShardingConfig` defaults to `enabled = false`; sharding is opt-in.

## example

```rust
use aiondb_core::RelationId;
use aiondb_shard::{HashRing, NodeAddress, ShardId, ShardKey, ShardManager, ShardingConfig};

let cfg = ShardingConfig::default();
assert!(!cfg.enabled);

let nodes = vec![
    NodeAddress::new(1, "127.0.0.1:5432"),
    NodeAddress::new(2, "127.0.0.1:5433"),
];
let manager = ShardManager::new_auto(
    RelationId::new(42),
    4,
    cfg.virtual_nodes_per_shard,
    &nodes,
)
.expect("auto shard manager");

let ring = HashRing::from_shards(
    &[ShardId::new(0), ShardId::new(1), ShardId::new(2)],
    cfg.virtual_nodes_per_shard,
);
let _owner = ring.lookup(&ShardKey::numeric(12_345).hash_bytes());

let _ = manager;
```
