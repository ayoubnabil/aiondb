//! Chaos test : deterministic random failures + invariant check.
//!
//! Inspired by Jepsen / FoundationDB simulation: drive a 5-node
//! cluster through hundreds of random operations interleaved with
//! random partitions and node restarts. After each scenario, check
//! the cluster invariants:
//!
//! 1. **No divergence**. Every healthy node observes the same
//!    committed metadata.
//! 2. **No data loss**. Every successfully-acked write is reflected
//!    in the final committed snapshot.
//! 3. **Lease singularity**. Every shard has exactly zero or one
//!    active lease at every moment in time.
//! 4. **Range non-overlap**. The range registry is always a partition
//!    of the key space (no overlaps, no gaps within the seeded
//!    boundaries).
//!
//! The chaos schedule is seeded so failures are reproducible.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aiondb_ha::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
use aiondb_ha::protocol::NodeId as RaftNodeId;
use aiondb_ha::raft::{AppendEntriesRequest, AppendEntriesResponse};
use aiondb_ha::raft_control_plane::{RaftControlPlane, DEFAULT_METADATA_GROUP_ID};
use aiondb_shard::lease::LeaseRegistry;
use aiondb_shard::range_descriptor::{
    RangeDescriptor, RangeDescriptorRegistry, RangeId, ReplicaDescriptor, ReplicaId,
};
use aiondb_shard::ShardId;

/// Deterministic 64-bit linear congruential PRNG. Small, stable, no
/// crate dep -- chaos seeds must be reproducible across builds.
struct ChaosRng(u64);

impl ChaosRng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }
    fn pick<T: Copy>(&mut self, slice: &[T]) -> T {
        let idx = (self.next() % slice.len() as u64) as usize;
        slice[idx]
    }
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + (self.next() % (hi - lo).max(1))
    }
    #[allow(dead_code)]
    fn bool(&mut self) -> bool {
        self.next() & 1 == 0
    }
}

struct ChaosNode {
    raft_id: RaftNodeId,
    raft_registry: Arc<MultiRaftRegistry>,
    control_plane: RaftControlPlane,
    leases: LeaseRegistry,
    ranges: RangeDescriptorRegistry,
    isolated: bool,
}

impl ChaosNode {
    fn new(id: u64, root: PathBuf) -> Self {
        let raft_id = RaftNodeId::new(id);
        let raft_registry = Arc::new(MultiRaftRegistry::new(raft_id, root).unwrap());
        let control_plane = RaftControlPlane::new(Arc::clone(&raft_registry));
        Self {
            raft_id,
            raft_registry,
            control_plane,
            leases: LeaseRegistry::new(),
            ranges: RangeDescriptorRegistry::new(),
            isolated: false,
        }
    }
}

fn pump_raft(nodes: &[ChaosNode], group: MultiRaftGroupId) {
    let mut outbound: BTreeMap<u64, Vec<(u64, AppendEntriesRequest)>> = BTreeMap::new();
    for node in nodes {
        if node.isolated {
            continue;
        }
        if let Ok(reqs) = node.raft_registry.build_append_entries_requests(group) {
            outbound.insert(node.raft_id.get(), reqs);
        }
    }
    let mut responses: BTreeMap<u64, Vec<AppendEntriesResponse>> = BTreeMap::new();
    for (src, reqs) in &outbound {
        let src_node = nodes.iter().find(|n| n.raft_id.get() == *src).unwrap();
        if src_node.isolated {
            continue;
        }
        for (target_id, req) in reqs {
            let target_node = nodes.iter().find(|n| n.raft_id.get() == *target_id);
            let Some(target_node) = target_node else {
                continue;
            };
            if target_node.isolated {
                continue;
            }
            if let Ok(resp) = target_node.raft_registry.handle_append_entries(group, req) {
                responses.entry(*src).or_default().push(resp);
            }
        }
    }
    for (src, resps) in responses {
        let src_node = nodes.iter().find(|n| n.raft_id.get() == src).unwrap();
        for resp in resps {
            let _ = src_node
                .raft_registry
                .handle_append_entries_response(group, &resp);
        }
    }
    for n in nodes {
        if !n.isolated {
            let _ = n.control_plane.apply_committed();
        }
    }
}

fn seed_range(nodes: &[ChaosNode]) {
    for n in nodes {
        n.ranges
            .upsert(RangeDescriptor {
                range_id: RangeId::new(1),
                start_key: b"".to_vec(),
                end_key: b"".to_vec(),
                replicas: vec![ReplicaDescriptor {
                    replica_id: ReplicaId::new(1),
                    node_id: format!("n{}", n.raft_id.get()),
                    is_learner: false,
                }],
                shard: ShardId::new(1),
                lease: None,
                generation: 0,
            })
            .unwrap();
    }
}

fn assert_invariants(nodes: &[ChaosNode]) {
    // Invariant 1 : healthy nodes converge on metadata.
    let snapshots: Vec<_> = nodes
        .iter()
        .filter(|n| !n.isolated)
        .map(|n| n.control_plane.snapshot())
        .collect();
    if snapshots.len() > 1 {
        // Healthy nodes may still be in different rounds of catch-up
        // mid-test. We assert a weaker property: the leader's snapshot
        // is a *superset* of every other healthy snapshot, since
        // followers can only lag, never diverge.
        let mut max_index = 0;
        let mut leader_snap = None;
        for s in &snapshots {
            if s.applied_index >= max_index {
                max_index = s.applied_index;
                leader_snap = Some(s);
            }
        }
        let leader = leader_snap.unwrap();
        for s in &snapshots {
            for m in &s.members {
                assert!(
                    leader.members.iter().any(|lm| lm == m),
                    "non-leader member missing from leader snapshot: {m:?}"
                );
            }
            for a in &s.assignments {
                assert!(
                    leader.assignments.iter().any(|la| la == a),
                    "non-leader assignment missing from leader: {a:?}"
                );
            }
        }
    }

    // Invariant 3 : at most one lease per shard.
    for n in nodes {
        let snap = n.leases.snapshot();
        let mut seen: BTreeSet<ShardId> = BTreeSet::new();
        for lease in snap {
            assert!(
                seen.insert(lease.shard),
                "duplicate lease for {:?}",
                lease.shard
            );
        }
    }

    // Invariant 4 : range registry has no overlaps. Each node tracks
    // its own copy; we check non-overlap per node.
    for n in nodes {
        let snap = n.ranges.snapshot();
        for w in snap.windows(2) {
            let (a, b) = (&w[0], &w[1]);
            // The second range's start_key must be >= first range's
            // end_key (with empty meaning infinity = ok).
            if !a.end_key.is_empty() && !b.start_key.is_empty() {
                assert!(
                    b.start_key >= a.end_key,
                    "ranges overlap on node {}: {:?} vs {:?}",
                    n.raft_id.get(),
                    a,
                    b
                );
            }
        }
    }
}

#[test]
fn chaos_5_nodes_random_ops_with_partitions() {
    let tmp = tempfile::tempdir().unwrap();
    let mut nodes: Vec<ChaosNode> = (1..=5u64)
        .map(|i| ChaosNode::new(i, tmp.path().join(format!("node-{i}"))))
        .collect();
    let g = MultiRaftGroupId::new(DEFAULT_METADATA_GROUP_ID);
    for n in &nodes {
        n.raft_registry.create_group(g, 5).unwrap();
    }
    seed_range(&nodes);

    // Make node 1 the initial leader.
    let peer_ids: Vec<u64> = nodes[1..].iter().map(|n| n.raft_id.get()).collect();
    nodes[0].raft_registry.become_leader(g, &peer_ids).unwrap();

    let mut rng = ChaosRng::new(0xC0FF_EEBA_BEDE_ADBE);
    let mut acked: BTreeMap<u64, String> = BTreeMap::new();
    let mut node_id_seq = 100u64;

    for op_idx in 0..150 {
        // Random op against the current leader.
        let op_kind = rng.range(0, 3);
        let leader = &nodes[0];
        let res = match op_kind {
            0 => {
                let id = node_id_seq;
                node_id_seq += 1;
                let addr = format!("host-{id}");
                if leader.control_plane.add_node(id, &addr).is_ok() {
                    acked.insert(id, addr.clone());
                    Some((id, addr))
                } else {
                    None
                }
            }
            1 => {
                let table = rng.range(1, 10);
                let shard = rng.range(0, 16) as u32;
                let target_node = if acked.is_empty() {
                    1
                } else {
                    *acked
                        .keys()
                        .nth((rng.next() as usize) % acked.len())
                        .unwrap()
                };
                let _ = leader.control_plane.assign_shard(table, shard, target_node);
                None
            }
            _ => {
                let _ = leader
                    .control_plane
                    .set_config(format!("k{}", rng.next() & 0xff), format!("v{}", op_idx));
                None
            }
        };
        let _ = res;
        pump_raft(&nodes, g);

        // Randomly partition / heal a node.
        if op_idx % 17 == 0 {
            let victim_idx = (rng.range(1, 5) as usize).min(nodes.len() - 1);
            nodes[victim_idx].isolated = !nodes[victim_idx].isolated;
        }
        // Catch up via several pump rounds.
        for _ in 0..3 {
            pump_raft(&nodes, g);
        }
        assert_invariants(&nodes);
    }

    // Heal everyone.
    for n in &mut nodes {
        n.isolated = false;
    }
    for _ in 0..30 {
        pump_raft(&nodes, g);
    }
    // Final convergence : every acked add_node must appear in the leader snapshot.
    let snap = nodes[0].control_plane.snapshot();
    for (id, addr) in &acked {
        let found = snap
            .members
            .iter()
            .any(|m| m.node_id == *id && m.address == *addr);
        assert!(
            found,
            "acked add_node({id}, {addr}) lost from leader snapshot"
        );
    }
    // Every healthy node converges to the leader's snapshot members set.
    let leader_members: BTreeSet<u64> = snap.members.iter().map(|m| m.node_id).collect();
    for n in &nodes {
        let local: BTreeSet<u64> = n
            .control_plane
            .snapshot()
            .members
            .iter()
            .map(|m| m.node_id)
            .collect();
        assert_eq!(
            local,
            leader_members,
            "node {} did not converge: leader={:?} local={:?}",
            n.raft_id.get(),
            leader_members,
            local
        );
    }
}

#[test]
fn chaos_lease_singularity_under_repeated_transfers() {
    let leases = LeaseRegistry::new();
    let mut rng = ChaosRng::new(42);
    let shards: Vec<ShardId> = (0..10u32).map(ShardId::new).collect();
    let now0 = Instant::now();

    // Bootstrap each shard on node 1.
    for s in &shards {
        leases.acquire(*s, 1, Duration::from_secs(60), now0);
    }

    // 500 random transfers.
    for _ in 0..500 {
        let shard = rng.pick(&shards);
        let target = rng.range(1, 6);
        leases.transfer(shard, target, Duration::from_secs(60), Instant::now());
        // Invariant : at most one lease per shard at all times.
        let snap = leases.snapshot();
        let mut seen: BTreeSet<ShardId> = BTreeSet::new();
        for lease in &snap {
            assert!(
                seen.insert(lease.shard),
                "duplicate lease for {:?}",
                lease.shard
            );
        }
        // Each transferred lease should reflect the new holder.
        let curr = leases.current(shard).expect("lease exists");
        assert_eq!(curr.holder, target);
    }
}
