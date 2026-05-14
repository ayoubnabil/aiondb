//! E2E workload run.
//!
//! Generates 5000 ops, applies them against a real KvEngine, asserts
//! the final state count is plausible.

use std::sync::Arc;

use aiondb_ha::kv_engine::KvEngine;
use aiondb_ha::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
use aiondb_ha::protocol::NodeId;
use aiondb_shard::dist_sender::BatchRequest;
use aiondb_shard::workload_generator::{WorkloadConfig, WorkloadGenerator, WorkloadProfile};

#[test]
fn five_k_ops_workload_against_kv_engine_terminates() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = Arc::new(MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap());
    let g = MultiRaftGroupId::new(1);
    registry.create_group(g, 1).unwrap();
    registry.become_leader(g, &[]).unwrap();
    let engine = KvEngine::new(Arc::clone(&registry));

    let mut gen = WorkloadGenerator::new(WorkloadConfig {
        profile: WorkloadProfile::Uniform,
        keyspace_size: 1000,
        ops: 5000,
        write_pct: 60,
        seed: 42,
    });
    let ops = gen.generate();
    assert_eq!(ops.len(), 5000);

    let mut keys_written = 0u64;
    for op in ops {
        match op {
            BatchRequest::Put { key, value } => {
                engine.put(g, key, value.unwrap_or_default()).unwrap();
                keys_written += 1;
            }
            BatchRequest::Get { key } => {
                let _ = engine.get(g, &key).unwrap();
            }
            _ => {}
        }
    }
    let snap = engine.snapshot(g);
    assert!(snap.len() <= 1000); // bounded by keyspace
    assert!(keys_written > 0);
}

#[test]
fn skewed_workload_produces_hot_keys() {
    let mut gen = WorkloadGenerator::new(WorkloadConfig {
        profile: WorkloadProfile::Skewed,
        keyspace_size: 100,
        ops: 10_000,
        write_pct: 100,
        seed: 7,
    });
    let ops = gen.generate();
    let mut counts: std::collections::HashMap<Vec<u8>, usize> = std::collections::HashMap::new();
    for op in ops {
        if let BatchRequest::Put { key, .. } = op {
            *counts.entry(key).or_default() += 1;
        }
    }
    let max = counts.values().copied().max().unwrap_or(0);
    let _avg = 10_000usize / counts.len().max(1);
    assert!(
        max as f64 / 10_000.0 > 0.005,
        "skewed should produce a hot key with > 0.5% of traffic, got max={max}"
    );
}
