//! End-to-end replicated KV test : 3 nodes share a single Raft group
//! for one KV "table". Drives writes through the leader and verifies
//! every follower converges to identical state, even across partition
//! + heal cycles.
//!
//! The transport is in-process (Raft messages routed by a manual pump
//! loop). This exercises the real Raft + multi-raft + kv_engine code
//! paths; only the network layer is stubbed.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use aiondb_ha::kv_engine::KvEngine;
use aiondb_ha::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
use aiondb_ha::protocol::NodeId;
use aiondb_ha::raft::{AppendEntriesRequest, AppendEntriesResponse, RaftRole};

struct KvNode {
    raft_id: NodeId,
    registry: Arc<MultiRaftRegistry>,
    engine: KvEngine,
    isolated: bool,
}

impl KvNode {
    fn new(id: u64, dir: PathBuf) -> Self {
        let raft_id = NodeId::new(id);
        let registry = Arc::new(MultiRaftRegistry::new(raft_id, dir).unwrap());
        let engine = KvEngine::new(Arc::clone(&registry));
        Self {
            raft_id,
            registry,
            engine,
            isolated: false,
        }
    }
}

fn pump(nodes: &[KvNode], group: MultiRaftGroupId) {
    let mut outbound: BTreeMap<u64, Vec<(u64, AppendEntriesRequest)>> = BTreeMap::new();
    for n in nodes {
        if n.isolated {
            continue;
        }
        if let Ok(reqs) = n.registry.build_append_entries_requests(group) {
            outbound.insert(n.raft_id.get(), reqs);
        }
    }
    let mut responses: BTreeMap<u64, Vec<AppendEntriesResponse>> = BTreeMap::new();
    for (src, reqs) in &outbound {
        let src_node = nodes.iter().find(|n| n.raft_id.get() == *src).unwrap();
        if src_node.isolated {
            continue;
        }
        for (target_id, req) in reqs {
            let target = nodes.iter().find(|n| n.raft_id.get() == *target_id);
            let Some(target) = target else {
                continue;
            };
            if target.isolated {
                continue;
            }
            if let Ok(resp) = target.registry.handle_append_entries(group, req) {
                responses.entry(*src).or_default().push(resp);
            }
        }
    }
    for (src, resps) in responses {
        let src_node = nodes.iter().find(|n| n.raft_id.get() == src).unwrap();
        for resp in resps {
            let _ = src_node
                .registry
                .handle_append_entries_response(group, &resp);
        }
    }
    for n in nodes {
        if !n.isolated {
            let _ = n.engine.apply_committed(group);
        }
    }
}

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn three_node_kv_replicates_writes_to_every_replica() {
    let tmp = tempdir();
    let nodes: Vec<KvNode> = (1..=3u64)
        .map(|i| KvNode::new(i, tmp.path().join(format!("node-{i}"))))
        .collect();
    let g = MultiRaftGroupId::new(7);
    for n in &nodes {
        n.registry.create_group(g, 3).unwrap();
    }
    let peer_ids: Vec<u64> = nodes[1..].iter().map(|n| n.raft_id.get()).collect();
    nodes[0].registry.become_leader(g, &peer_ids).unwrap();

    // Write 50 keys through the leader.
    for i in 0..50u8 {
        let _ = nodes[0].engine.put(
            g,
            format!("k{i}").into_bytes(),
            format!("v{i}").into_bytes(),
        );
    }
    // Replicate.
    for _ in 0..20 {
        pump(&nodes, g);
    }
    // Followers must observe every value.
    for n in &nodes {
        let snap = n.engine.snapshot(g);
        assert_eq!(snap.len(), 50, "node {} missing values", n.raft_id.get());
        for i in 0..50u8 {
            let key = format!("k{i}").into_bytes();
            let want = format!("v{i}").into_bytes();
            assert_eq!(
                snap.get(&key),
                Some(&want),
                "node {} key {i}",
                n.raft_id.get()
            );
        }
    }
}

#[test]
fn partition_isolates_writes_then_heals_correctly() {
    let tmp = tempdir();
    let mut nodes: Vec<KvNode> = (1..=3u64)
        .map(|i| KvNode::new(i, tmp.path().join(format!("node-{i}"))))
        .collect();
    let g = MultiRaftGroupId::new(7);
    for n in &nodes {
        n.registry.create_group(g, 3).unwrap();
    }
    let peer_ids: Vec<u64> = nodes[1..].iter().map(|n| n.raft_id.get()).collect();
    nodes[0].registry.become_leader(g, &peer_ids).unwrap();

    nodes[0]
        .engine
        .put(g, b"pre".to_vec(), b"value0".to_vec())
        .unwrap();
    for _ in 0..10 {
        pump(&nodes, g);
    }
    for n in &nodes {
        assert_eq!(n.engine.get(g, b"pre").unwrap(), Some(b"value0".to_vec()));
    }

    // Isolate node 3.
    nodes[2].isolated = true;
    // Leader writes 20 more keys.
    for i in 0..20u8 {
        nodes[0]
            .engine
            .put(
                g,
                format!("k{i}").into_bytes(),
                format!("v{i}").into_bytes(),
            )
            .unwrap();
    }
    for _ in 0..15 {
        pump(&nodes, g);
    }
    // Nodes 1+2 caught up, node 3 is stuck on pre-partition state.
    assert_eq!(nodes[0].engine.snapshot(g).len(), 21);
    assert_eq!(nodes[1].engine.snapshot(g).len(), 21);
    assert_eq!(
        nodes[2].engine.snapshot(g).len(),
        1,
        "isolated node should still see only the pre-partition write"
    );

    // Heal the partition.
    nodes[2].isolated = false;
    for _ in 0..30 {
        pump(&nodes, g);
    }
    // Node 3 catches up.
    let s3 = nodes[2].engine.snapshot(g);
    assert_eq!(s3.len(), 21, "node 3 should converge: {s3:?}");
    for i in 0..20u8 {
        let key = format!("k{i}").into_bytes();
        let want = format!("v{i}").into_bytes();
        assert_eq!(s3.get(&key), Some(&want));
    }
}

#[test]
fn cas_under_replication_preserves_consistency() {
    let tmp = tempdir();
    let nodes: Vec<KvNode> = (1..=3u64)
        .map(|i| KvNode::new(i, tmp.path().join(format!("node-{i}"))))
        .collect();
    let g = MultiRaftGroupId::new(42);
    for n in &nodes {
        n.registry.create_group(g, 3).unwrap();
    }
    let peer_ids: Vec<u64> = nodes[1..].iter().map(|n| n.raft_id.get()).collect();
    nodes[0].registry.become_leader(g, &peer_ids).unwrap();

    // Seed counter at "0".
    nodes[0]
        .engine
        .put(g, b"counter".to_vec(), b"0".to_vec())
        .unwrap();
    for _ in 0..5 {
        pump(&nodes, g);
    }

    // 50 increments on the leader via CAS loop. Pump after every
    // attempt so the new value is replicated + applied before the next
    // read.
    for _ in 0..50 {
        loop {
            // Drain pending commits before reading.
            for _ in 0..5 {
                pump(&nodes, g);
            }
            let current = nodes[0].engine.get(g, b"counter").unwrap();
            let parsed: i64 = std::str::from_utf8(current.as_ref().unwrap())
                .unwrap()
                .parse()
                .unwrap();
            let next = (parsed + 1).to_string().into_bytes();
            let ok = nodes[0]
                .engine
                .cas(g, b"counter".to_vec(), current, Some(next))
                .unwrap();
            if ok {
                // Replicate before letting the next iteration read.
                for _ in 0..5 {
                    pump(&nodes, g);
                }
                break;
            }
        }
    }
    for _ in 0..20 {
        pump(&nodes, g);
    }
    for n in &nodes {
        let val = n.engine.get(g, b"counter").unwrap().unwrap();
        assert_eq!(
            std::str::from_utf8(&val).unwrap(),
            "50",
            "node {} counter must be 50",
            n.raft_id.get()
        );
    }
}

#[test]
fn leader_only_can_propose_writes() {
    let tmp = tempdir();
    let nodes: Vec<KvNode> = (1..=3u64)
        .map(|i| KvNode::new(i, tmp.path().join(format!("node-{i}"))))
        .collect();
    let g = MultiRaftGroupId::new(1);
    for n in &nodes {
        n.registry.create_group(g, 3).unwrap();
    }
    let peer_ids: Vec<u64> = nodes[1..].iter().map(|n| n.raft_id.get()).collect();
    nodes[0].registry.become_leader(g, &peer_ids).unwrap();

    // Follower (node 2) cannot propose.
    let err = nodes[1]
        .engine
        .put(g, b"k".to_vec(), b"v".to_vec())
        .unwrap_err();
    assert!(err.to_string().contains("not led"), "err: {err}");

    // Leader still works.
    nodes[0]
        .engine
        .put(g, b"k".to_vec(), b"v".to_vec())
        .unwrap();
    let role = nodes[0].registry.group_state(g).unwrap().role;
    assert_eq!(role, RaftRole::Leader);
}
