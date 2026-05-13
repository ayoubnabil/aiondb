---
title: aiondb-ha
order: 61
---

# aiondb-ha

High-availability subsystem for AionDB. Provides epoch-based leader election, inter-node heartbeats, fencing tokens to guard against split-brain, a failover orchestrator, and a Raft consensus layer for cluster metadata. The Raft layer extends the election subsystem with persistent `voted_for`, `AppendEntries` log replication, and a durable JSON-lines log of `RaftCommand` entries.

This crate is the **control plane** half of replication. The data plane -- per-record WAL streaming from primary to replica, `apply_lsn` accounting, and the hot-standby replay loop -- lives in [`aiondb-replication`](/specification-avancee/aiondb-replication.html) and runs on the pgwire listener. Raft never carries SQL or DDL records: its commands are limited to topology (`AddNode`, `RemoveNode`), shard placement (`AssignShard`, `TransferShard`), cluster settings (`UpdateConfig`), and the internal HA key-value engine (`KvWrite`). User table writes always flow through the WAL stream.

`aiondb-server` wires this crate behind `config.ha.enabled` and binds it to `config.ha.ha_port`, distinct from the pgwire port. On `FailoverEvent::ElectionWon` the orchestrator calls `HaIntegration::promote()`, which in turn flips `StreamingReplicationState` to `Primary` so the engine starts accepting SQL writes.

## cargo

```toml
[dependencies]
aiondb-ha = { path = "../aiondb-ha" }
```

## modules

| module | purpose |
|---|---|
| `protocol` | wire types, HMAC framing (`encode_authenticated`, `decode_authenticated`), `NodeId`, `Epoch`, `NodeRole`, `HaMessage`. |
| `health` | `HealthMonitor`, `NodeHealth`, `PrimaryHealthStatus`. |
| `election` | epoch-based leader election keyed by highest LSN. |
| `fencing` | `FencingToken`, `FencingGuard`. |
| `failover` | `FailoverOrchestrator`, `FailoverState`, `FailoverEvent`, `DirectedHaMessage`. |
| `raft` | Raft node, log, RPC, and persistent state for metadata consensus. |

## raft submodule

| file | contents |
|---|---|
| `raft/state.rs` | `PersistentState`, `RaftCommand` (`Noop`, `AddNode`, `RemoveNode`, `AssignShard`, `TransferShard`, `UpdateConfig`, `KvWrite`). |
| `raft/log.rs` | `RaftEntry`, `RaftLog` (JSON-lines file with contiguity check). |
| `raft/rpc.rs` | `AppendEntriesRequest`, `AppendEntriesResponse`. |
| `raft/node.rs` | `RaftNode`, `RaftRole` (`Follower`, `Candidate`, `Leader`). |

`RaftNode::open` restores or creates `raft_state.json` and `raft_log.jsonl` under the supplied state directory. The node does not own network I/O; the caller is responsible for delivering messages produced and accepted by it.

## key types

| type | role |
|---|---|
| `NodeId`, `Epoch`, `NodeRole`, `HaMessage` | base wire types. |
| `compute_hmac`, `verify_hmac`, `encode_authenticated`, `decode_authenticated` | HMAC-SHA256 framing helpers. |
| `HealthMonitor`, `NodeHealth`, `PrimaryHealthStatus` | heartbeat tracking. |
| `LeaderElection`, `ElectionResult` | quorum vote driver; results are `Won`, `Lost`, `Timeout`, `InsufficientNodes`. |
| `FencingToken`, `FencingGuard` | epoch-monotonic guard against split-brain. |
| `FailoverOrchestrator`, `FailoverState`, `FailoverEvent`, `DirectedHaMessage` | high-level orchestration of demotion and promotion. |
| `RaftNode`, `RaftRole`, `RaftEntry`, `RaftLog`, `RaftCommand`, `PersistentState`, `AppendEntriesRequest`, `AppendEntriesResponse` | Raft layer. |

## example

```rust
use std::time::Duration;

use aiondb_ha::{HealthMonitor, NodeId, PrimaryHealthStatus};

let local = NodeId::new(1);
let monitor = HealthMonitor::new(local, Duration::from_secs(5));

match monitor.check_primary_health() {
    PrimaryHealthStatus::Healthy => {}
    PrimaryHealthStatus::Unreachable { .. } => {}
    PrimaryHealthStatus::Unknown => {}
}
```

```rust
use std::path::PathBuf;

use aiondb_ha::{NodeId, RaftNode, RaftRole};

let dir = PathBuf::from("/tmp/aiondb-raft-demo");
std::fs::create_dir_all(&dir).unwrap();
let node = RaftNode::open(NodeId::new(1), 3, dir).expect("open raft node");
assert!(matches!(node.role(), RaftRole::Follower));
```
